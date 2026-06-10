//! Live Activity Feed — chronological "what is happening" screen.
//!
//! Reads from [`EventStoreCtx`]: history is already present in
//! `all_events` on first render (catch-up handled by ws.rs).  Deep history
//! (beyond the catch-up window) is loaded lazily via
//! [`api::events_since`] when the user scrolls to the top or clicks
//! "Load older".
//!
//! # Layout
//!
//! ```text
//! ┌─ .activity-feed ──────────────────────────────────────────────────────┐
//! │ .feed-header  [title] [conn-badge]  [filter-bar ▾]                   │
//! │ .feed-filters (collapsible)                                           │
//! │ .feed-list                                                            │
//! │   .feed-load-older  (only if history not fully loaded)                │
//! │   .feed-entry  ×N  (or .feed-burst for grouped entries)               │
//! │ .feed-new-chip  (floats, visible when scrolled away from bottom)      │
//! └───────────────────────────────────────────────────────────────────────┘
//! ```

use crate::api;
use crate::event_store::{ConnState, EventStoreCtx};
use leptos::prelude::*;
use taskagent_events::{Channel, Event, EventEnvelope};
use taskagent_shared::time::Timestamp;
use wasm_bindgen_futures::spawn_local;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of entries rendered in the DOM at once.
const WINDOW_SIZE: usize = 200;

/// Number of consecutive events from the same actor that triggers burst
/// grouping.
const BURST_THRESHOLD: usize = 3;

/// Page size for the "load older" deep-history fetch.
const HISTORY_PAGE: usize = 200;

// ── Filter state ──────────────────────────────────────────────────────────────

/// Which actor types to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActorFilter {
    All,
    HumanOnly,
    AgentOnly,
}

/// Relative time window for filtering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeWindow {
    Last5Min,
    LastHour,
    All,
}

/// All channel variants in display order.
const ALL_CHANNELS: &[Channel] = &[
    Channel::Tasks,
    Channel::Plans,
    Channel::Documents,
    Channel::Runs,
    Channel::Comments,
    Channel::AgentStatus,
    Channel::AiOps,
    Channel::WorkUnits,
    Channel::Artifacts,
    Channel::Presence,
    Channel::Webhooks,
];

fn channel_label(ch: Channel) -> &'static str {
    match ch {
        Channel::Tasks => "Tasks",
        Channel::Plans => "Plans",
        Channel::Documents => "Docs",
        Channel::Runs => "Runs",
        Channel::Comments => "Comments",
        Channel::AgentStatus => "Agent",
        Channel::AiOps => "AI Ops",
        Channel::WorkUnits => "Work Units",
        Channel::Artifacts => "Artifacts",
        Channel::Presence => "Presence",
        Channel::Webhooks => "Webhooks",
    }
}

// ── Entry model ───────────────────────────────────────────────────────────────

/// A single processed feed row derived from an [`EventEnvelope`].
#[derive(Clone, Debug, PartialEq)]
struct FeedEntry {
    seq: u64,
    occurred_at: Timestamp,
    /// Display name of the actor (e.g. "user", agent handle, or email).
    actor_label: String,
    /// Whether the actor is an automated agent.
    is_agent: bool,
    channel: Channel,
    /// Short human-readable summary of what happened.
    summary: String,
    /// Optional entity id for link construction.
    entity_id: Option<String>,
}

/// A single row in the rendered feed — either one entry or a burst group.
#[derive(Clone, Debug, PartialEq)]
enum FeedRow {
    Single(FeedEntry),
    /// N consecutive events by the same actor, collapsed into one row.
    Burst {
        actor_label: String,
        is_agent: bool,
        count: usize,
        first_seq: u64,
        occurred_at: Timestamp,
        channel: Channel,
    },
}

impl FeedRow {
    fn seq(&self) -> u64 {
        match self {
            FeedRow::Single(e) => e.seq,
            FeedRow::Burst { first_seq, .. } => *first_seq,
        }
    }
}

// ── EventEnvelope → FeedEntry ─────────────────────────────────────────────────

fn actor_label(env: &EventEnvelope) -> String {
    use taskagent_domain::Actor;
    match &env.actor {
        Actor::User => "user".to_string(),
        Actor::Agent { name, .. } => name.clone(),
    }
}

fn is_agent_actor(env: &EventEnvelope) -> bool {
    use taskagent_domain::Actor;
    matches!(&env.actor, Actor::Agent { .. })
}

fn entry_summary(env: &EventEnvelope) -> (String, Option<String>) {
    match &env.payload {
        // ── Tasks ─────────────────────────────────────────────────────────────
        Event::TaskCreated { task } => {
            let title = task.title.chars().take(60).collect::<String>();
            let entity = task.id.map(|id| id.to_string());
            (format!("created task \"{title}\""), entity)
        }
        Event::TaskUpdated { task_id, patch } => {
            let what = if patch.title.is_some() {
                "renamed"
            } else if patch.status.is_some() {
                "updated status on"
            } else {
                "updated"
            };
            (format!("{what} task"), Some(task_id.to_string()))
        }
        Event::TaskStatusChanged { task_id, from, to } => (
            format!("changed task status {from:?} → {to:?}"),
            Some(task_id.to_string()),
        ),
        Event::TaskPriorityChanged { task_id, from, to } => (
            format!("changed task priority {from:?} → {to:?}"),
            Some(task_id.to_string()),
        ),
        Event::TaskCompleted { task_id, .. } => {
            (format!("completed task"), Some(task_id.to_string()))
        }
        Event::TaskDeleted { task_id } => (format!("deleted task"), Some(task_id.to_string())),
        Event::TaskClosed { task_id, .. } => (format!("closed task"), Some(task_id.to_string())),
        Event::TaskReopened { task_id, .. } => {
            (format!("reopened task"), Some(task_id.to_string()))
        }
        Event::TaskCommented {
            task_id, preview, ..
        } => {
            let snippet = preview.chars().take(80).collect::<String>();
            (
                format!("commented: \"{snippet}\""),
                Some(task_id.to_string()),
            )
        }
        Event::TaskLinked { .. } => (format!("linked tasks"), None),
        Event::TaskUnlinked { .. } => (format!("unlinked tasks"), None),
        Event::TaskUnblocked { task_id, .. } => {
            (format!("unblocked task"), Some(task_id.to_string()))
        }
        Event::TaskSplitGenerated { parent, subtasks } => (
            format!("split task into {} subtasks", subtasks.len()),
            Some(parent.to_string()),
        ),
        // ── Plans ─────────────────────────────────────────────────────────────
        Event::PlanCreated { plan } => {
            let title = plan.title.chars().take(60).collect::<String>();
            (
                format!("created plan \"{title}\""),
                Some(plan.id.to_string()),
            )
        }
        Event::PlanUpdated { plan_id, .. } => (format!("updated plan"), Some(plan_id.to_string())),
        Event::PlanStatusChanged { plan_id, from, to } => (
            format!("changed plan status {from:?} → {to:?}"),
            Some(plan_id.to_string()),
        ),
        Event::PlanGoalChanged { plan_id, .. } => {
            (format!("changed plan goal"), Some(plan_id.to_string()))
        }
        Event::PlanTaskAdded {
            plan_id, task_id, ..
        } => (
            format!("added task to plan"),
            Some(format!("{plan_id}/{task_id}")),
        ),
        Event::PlanTaskRemoved { plan_id, .. } => {
            (format!("removed task from plan"), Some(plan_id.to_string()))
        }
        Event::PlanReordered { plan_id, .. } => {
            (format!("reordered plan tasks"), Some(plan_id.to_string()))
        }
        Event::PlanArchived { plan_id, .. } => {
            (format!("archived plan"), Some(plan_id.to_string()))
        }
        // ── Runs ──────────────────────────────────────────────────────────────
        Event::RunStarted { run } => (format!("started run"), Some(run.id.to_string())),
        Event::RunStepStarted {
            run_id, task_id, ..
        } => (
            format!("run step started on task"),
            Some(format!("{run_id}/{task_id}")),
        ),
        Event::RunStepFinished { run_id, .. } => {
            (format!("run step finished"), Some(run_id.to_string()))
        }
        Event::RunCompleted { run_id, .. } => (format!("run completed"), Some(run_id.to_string())),
        Event::RunFailed { run_id, reason, .. } => (
            format!(
                "run failed: {}",
                reason.chars().take(60).collect::<String>()
            ),
            Some(run_id.to_string()),
        ),
        Event::RunAborted { run_id, reason, .. } => (
            format!(
                "run aborted: {}",
                reason.chars().take(60).collect::<String>()
            ),
            Some(run_id.to_string()),
        ),
        Event::RunUnresponsive { run_id, .. } => {
            (format!("run unresponsive"), Some(run_id.to_string()))
        }
        Event::RunStale { run_id, .. } => (format!("run stale"), Some(run_id.to_string())),
        Event::RunNoteAppended { run_id, body, .. } => (
            format!(
                "appended run note: \"{}\"",
                body.chars().take(60).collect::<String>()
            ),
            Some(run_id.to_string()),
        ),
        // ── Documents ─────────────────────────────────────────────────────────
        Event::DocumentCreated { document } => {
            let title = document.title.chars().take(60).collect::<String>();
            (
                format!("created document \"{title}\""),
                Some(document.id.to_string()),
            )
        }
        Event::DocumentContentReplaced { document_id, .. } => (
            format!("replaced document content"),
            Some(document_id.to_string()),
        ),
        Event::DocumentContentAppended { document_id, .. } => (
            format!("appended to document"),
            Some(document_id.to_string()),
        ),
        Event::DocumentRenamed {
            document_id, title, ..
        } => (
            format!(
                "renamed document to \"{}\"",
                title.chars().take(60).collect::<String>()
            ),
            Some(document_id.to_string()),
        ),
        Event::DocumentArchived { document_id, .. } => {
            (format!("archived document"), Some(document_id.to_string()))
        }
        // ── Comments ──────────────────────────────────────────────────────────
        Event::CommentAdded { comment } => {
            let preview = comment.body.chars().take(80).collect::<String>();
            (
                format!("commented: \"{preview}\""),
                Some(comment.task_id.to_string()),
            )
        }
        Event::CommentEdited { task_id, .. } => {
            (format!("edited comment"), Some(task_id.to_string()))
        }
        Event::CommentDeleted { task_id, .. } => {
            (format!("deleted comment"), Some(task_id.to_string()))
        }
        // ── Projects ──────────────────────────────────────────────────────────
        Event::ProjectCreated { project } => {
            let title = project.title.chars().take(60).collect::<String>();
            (
                format!("created project \"{title}\""),
                Some(project.id.to_string()),
            )
        }
        Event::ProjectUpdated {
            project_id, title, ..
        } => {
            let suffix = title
                .as_ref()
                .map(|t| format!(" \"{t}\""))
                .unwrap_or_default();
            (
                format!("updated project{suffix}"),
                Some(project_id.to_string()),
            )
        }
        Event::ProjectDeleted { project_id } => {
            (format!("deleted project"), Some(project_id.to_string()))
        }
        // ── Agent / AI ────────────────────────────────────────────────────────
        Event::AgentSessionStarted { .. } => (format!("agent session started"), None),
        Event::AgentSessionEnded { .. } => (format!("agent session ended"), None),
        Event::AgentActionRecorded { .. } => (format!("agent action recorded"), None),
        Event::AiOperationStarted { .. } => (format!("AI operation started"), None),
        Event::AiOperationPhaseChanged { .. } => (format!("AI operation phase changed"), None),
        Event::AiOperationCompleted { .. } => (format!("AI operation completed"), None),
        // ── Work Units ────────────────────────────────────────────────────────
        Event::WorkUnitCreated { .. } => (format!("work unit created"), None),
        Event::WorkUnitClaimed { .. } => (format!("work unit claimed"), None),
        Event::WorkUnitStarted { .. } => (format!("work unit started"), None),
        Event::WorkUnitBlocked { .. } => (format!("work unit blocked"), None),
        Event::WorkUnitCompleted { .. } => (format!("work unit completed"), None),
        Event::WorkUnitReleased { .. } => (format!("work unit released"), None),
        // ── Catch-all ─────────────────────────────────────────────────────────
        other => (other.kind().replace('_', " "), None),
    }
}

fn envelope_to_entry(env: &EventEnvelope) -> FeedEntry {
    let (summary, entity_id) = entry_summary(env);
    FeedEntry {
        seq: env.seq,
        occurred_at: env.occurred_at,
        actor_label: actor_label(env),
        is_agent: is_agent_actor(env),
        channel: env.payload.channel(),
        summary,
        entity_id,
    }
}

// ── Burst grouping ────────────────────────────────────────────────────────────

fn build_rows(entries: Vec<FeedEntry>) -> Vec<FeedRow> {
    let mut rows: Vec<FeedRow> = Vec::with_capacity(entries.len());
    let mut i = 0usize;
    while i < entries.len() {
        // Count consecutive entries from the same actor.
        let actor = &entries[i].actor_label;
        let mut j = i + 1;
        while j < entries.len() && &entries[j].actor_label == actor {
            j += 1;
        }
        let run = j - i;
        if run >= BURST_THRESHOLD {
            rows.push(FeedRow::Burst {
                actor_label: actor.clone(),
                is_agent: entries[i].is_agent,
                count: run,
                first_seq: entries[i].seq,
                occurred_at: entries[i].occurred_at,
                channel: entries[i].channel,
            });
            i = j;
        } else {
            rows.push(FeedRow::Single(entries[i].clone()));
            i += 1;
        }
    }
    rows
}

// ── Time formatting ───────────────────────────────────────────────────────────

fn format_time(ts: Timestamp) -> String {
    // Timestamp is chrono::DateTime<Utc> via taskagent_shared::time.
    use chrono::Timelike;
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second())
}

fn ts_millis(ts: Timestamp) -> i64 {
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    dt.timestamp_millis()
}

// ── Connection badge ──────────────────────────────────────────────────────────

fn conn_badge_class(state: &ConnState) -> &'static str {
    match state {
        ConnState::Live => "conn-badge conn-badge--live",
        ConnState::CatchingUp => "conn-badge conn-badge--catching-up",
        ConnState::Connecting => "conn-badge conn-badge--connecting",
        ConnState::Offline { .. } => "conn-badge conn-badge--offline",
    }
}

fn conn_badge_label(state: &ConnState) -> String {
    match state {
        ConnState::Live => "live".to_string(),
        ConnState::CatchingUp => "catching up…".to_string(),
        ConnState::Connecting => "connecting…".to_string(),
        ConnState::Offline { retry_in_secs } => format!("offline (retry in {retry_in_secs}s)"),
    }
}

// ── Channel badge ─────────────────────────────────────────────────────────────

fn channel_class(ch: Channel) -> &'static str {
    match ch {
        Channel::Tasks => "ch-badge ch-tasks",
        Channel::Plans => "ch-badge ch-plans",
        Channel::Documents => "ch-badge ch-docs",
        Channel::Runs => "ch-badge ch-runs",
        Channel::Comments => "ch-badge ch-comments",
        Channel::AgentStatus => "ch-badge ch-agent",
        Channel::AiOps => "ch-badge ch-aiops",
        Channel::WorkUnits => "ch-badge ch-workunits",
        Channel::Artifacts => "ch-badge ch-artifacts",
        Channel::Presence => "ch-badge ch-presence",
        Channel::Webhooks => "ch-badge ch-webhooks",
    }
}

// ── Component ─────────────────────────────────────────────────────────────────

/// Full-screen live activity feed.  Mount in place of `<TaskList />` when
/// the `/activity` route is active; wired in by `app.rs`.
#[component]
pub fn ActivityFeed() -> impl IntoView {
    let store = use_context::<EventStoreCtx>().expect("EventStoreCtx");

    // ── Filter signals ────────────────────────────────────────────────────────
    // A set of enabled channels.  `None` means "all channels".
    let channel_filter: RwSignal<Option<std::collections::HashSet<Channel>>> = RwSignal::new(None);
    let actor_filter: RwSignal<ActorFilter> = RwSignal::new(ActorFilter::All);
    let time_window: RwSignal<TimeWindow> = RwSignal::new(TimeWindow::All);
    let filters_open: RwSignal<bool> = RwSignal::new(false);

    // ── Deep history loading ──────────────────────────────────────────────────
    // `history_loaded` — we've already fetched everything before the WS cursor.
    let history_loaded: RwSignal<bool> = RwSignal::new(false);
    let history_loading: RwSignal<bool> = RwSignal::new(false);
    // Extra historical events fetched via api::events_since, prepended.
    let history_events: RwSignal<Vec<EventEnvelope>> = RwSignal::new(Vec::new());

    // ── "N new" chip ─────────────────────────────────────────────────────────
    // Count of events that arrived while user was scrolled away from bottom.
    let new_count: RwSignal<usize> = RwSignal::new(0);
    // Whether the user is pinned to the bottom (auto-scroll).
    let pinned: RwSignal<bool> = RwSignal::new(true);
    // Track how many events were in the last render to detect new arrivals.
    let last_seen_len: RwSignal<usize> = RwSignal::new(0);

    // When new events arrive and we're not pinned, increment new_count.
    Effect::new(move |_| {
        let current_len = store.all_events.with(|v| v.len());
        let seen = last_seen_len.get_untracked();
        if current_len > seen {
            let delta = current_len - seen;
            if !pinned.get_untracked() {
                new_count.update(|n| *n += delta);
            }
            last_seen_len.set(current_len);
        }
    });

    // ── Derived: filtered + windowed rows ─────────────────────────────────────
    let visible_rows = Memo::new(move |_| {
        let all = store.all_events.get();
        let hist = history_events.get();
        let ch_filter = channel_filter.get();
        let af = actor_filter.get();
        let tw = time_window.get();

        // Combine history (older) + live events (newer).
        let combined: Vec<&EventEnvelope> = hist.iter().chain(all.iter()).collect();

        // Cutoff timestamp for time window.
        let now_ms = {
            use chrono::Utc;
            Utc::now().timestamp_millis()
        };
        let cutoff_ms: Option<i64> = match tw {
            TimeWindow::Last5Min => Some(now_ms - 5 * 60 * 1000),
            TimeWindow::LastHour => Some(now_ms - 60 * 60 * 1000),
            TimeWindow::All => None,
        };

        let filtered: Vec<FeedEntry> = combined
            .iter()
            .filter(|env| {
                // Channel filter.
                if let Some(ref enabled) = ch_filter {
                    if !enabled.contains(&env.payload.channel()) {
                        return false;
                    }
                }
                // Actor filter.
                match af {
                    ActorFilter::All => {}
                    ActorFilter::HumanOnly => {
                        if is_agent_actor(env) {
                            return false;
                        }
                    }
                    ActorFilter::AgentOnly => {
                        if !is_agent_actor(env) {
                            return false;
                        }
                    }
                }
                // Time window.
                if let Some(cutoff) = cutoff_ms {
                    if ts_millis(env.occurred_at) < cutoff {
                        return false;
                    }
                }
                true
            })
            .map(|env| envelope_to_entry(env))
            .collect();

        // Window to last WINDOW_SIZE entries.
        let windowed = if filtered.len() > WINDOW_SIZE {
            filtered[filtered.len() - WINDOW_SIZE..].to_vec()
        } else {
            filtered
        };

        build_rows(windowed)
    });

    // ── Load-older handler ────────────────────────────────────────────────────
    let load_older = move |_: web_sys::MouseEvent| {
        if history_loading.get_untracked() || history_loaded.get_untracked() {
            return;
        }
        history_loading.set(true);

        // Find the oldest seq we have (history first, then live).
        let oldest_seq = history_events
            .with_untracked(|h| h.first().map(|e| e.seq))
            .or_else(|| {
                store
                    .all_events
                    .with_untracked(|v| v.first().map(|e| e.seq))
            })
            .unwrap_or(0);

        // Fetch the page before `oldest_seq`.
        let since = if oldest_seq > 0 {
            oldest_seq.saturating_sub(HISTORY_PAGE as u64)
        } else {
            0
        };

        spawn_local(async move {
            match api::events_since(since, HISTORY_PAGE).await {
                Ok(mut page) => {
                    // Keep only events older than what we already have.
                    let cutoff = oldest_seq;
                    page.retain(|e| e.seq < cutoff);
                    if page.len() < HISTORY_PAGE {
                        history_loaded.set(true);
                    }
                    if !page.is_empty() {
                        history_events.update(|h| {
                            page.extend(h.drain(..));
                            *h = page;
                        });
                    }
                }
                Err(e) => {
                    leptos::logging::warn!("[activity] load_older failed: {e}");
                    history_loaded.set(true); // stop retrying on error
                }
            }
            history_loading.set(false);
        });
    };

    // ── "Jump to bottom" ──────────────────────────────────────────────────────
    let jump_to_bottom = move |_: web_sys::MouseEvent| {
        pinned.set(true);
        new_count.set(0);
        // Scroll the feed list to bottom via JS.
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                if let Some(el) = document.get_element_by_id("feed-list") {
                    el.set_scroll_top(i32::MAX);
                }
            }
        }
    };

    // ── Scroll handler — detect when user leaves the bottom ───────────────────
    let on_scroll = move |_: web_sys::Event| {
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                if let Some(el) = document.get_element_by_id("feed-list") {
                    let scroll_top = el.scroll_top();
                    let scroll_height = el.scroll_height();
                    let client_height = el.client_height();
                    let at_bottom = scroll_height - scroll_top - client_height < 40;
                    if at_bottom {
                        pinned.set(true);
                        new_count.set(0);
                    } else {
                        pinned.set(false);
                    }
                }
            }
        }
    };

    view! {
        <div class="activity-feed">
            // ── Header ────────────────────────────────────────────────────────
            <div class="feed-header">
                <span class="feed-title">"Activity"</span>
                <ConnBadge store=store />
                <button
                    class="feed-filter-toggle"
                    type="button"
                    on:click=move |_| filters_open.update(|o| *o = !*o)
                    aria-expanded=move || filters_open.get().to_string()
                >
                    { move || if filters_open.get() { "▴ Filters" } else { "▾ Filters" } }
                </button>
            </div>

            // ── Filter bar ────────────────────────────────────────────────────
            <Show when=move || filters_open.get() fallback=|| ()>
                <div class="feed-filters">
                    // Time window buttons.
                    <div class="filter-group">
                        <span class="filter-label">"Window"</span>
                        { [
                            (TimeWindow::Last5Min, "5 min"),
                            (TimeWindow::LastHour, "1 hr"),
                            (TimeWindow::All,       "All"),
                        ].map(|(tw_val, label)| view! {
                            <button
                                class=move || {
                                    if time_window.get() == tw_val {
                                        "filter-btn filter-btn--active"
                                    } else {
                                        "filter-btn"
                                    }
                                }
                                type="button"
                                on:click=move |_| time_window.set(tw_val)
                            >
                                { label }
                            </button>
                        }).collect_view() }
                    </div>

                    // Actor type buttons.
                    <div class="filter-group">
                        <span class="filter-label">"Actor"</span>
                        { [
                            (ActorFilter::All,       "All"),
                            (ActorFilter::HumanOnly, "Human"),
                            (ActorFilter::AgentOnly, "Agent"),
                        ].map(|(af_val, label)| view! {
                            <button
                                class=move || {
                                    if actor_filter.get() == af_val {
                                        "filter-btn filter-btn--active"
                                    } else {
                                        "filter-btn"
                                    }
                                }
                                type="button"
                                on:click=move |_| actor_filter.set(af_val)
                            >
                                { label }
                            </button>
                        }).collect_view() }
                    </div>

                    // Channel checkboxes.
                    <div class="filter-group filter-group--channels">
                        <span class="filter-label">"Channels"</span>
                        <button
                            class="filter-btn"
                            type="button"
                            on:click=move |_| channel_filter.set(None)
                        >
                            "All"
                        </button>
                        { ALL_CHANNELS.iter().map(|&ch| {
                            let is_enabled = move || channel_filter.with(|f| {
                                f.as_ref().map(|set| set.contains(&ch)).unwrap_or(true)
                            });
                            let toggle = move |_: web_sys::MouseEvent| {
                                channel_filter.update(|f| {
                                    let set = f.get_or_insert_with(|| {
                                        ALL_CHANNELS.iter().copied().collect()
                                    });
                                    if set.contains(&ch) {
                                        set.remove(&ch);
                                    } else {
                                        set.insert(ch);
                                    }
                                    // If all are selected, go back to None (= all).
                                    if set.len() == ALL_CHANNELS.len() {
                                        *f = None;
                                    }
                                });
                            };
                            view! {
                                <button
                                    class=move || {
                                        if is_enabled() {
                                            "filter-btn filter-btn--ch filter-btn--active"
                                        } else {
                                            "filter-btn filter-btn--ch"
                                        }
                                    }
                                    type="button"
                                    on:click=toggle
                                >
                                    { channel_label(ch) }
                                </button>
                            }
                        }).collect_view() }
                    </div>
                </div>
            </Show>

            // ── Feed list ─────────────────────────────────────────────────────
            <div
                id="feed-list"
                class="feed-list"
                on:scroll=on_scroll
            >
                // Load-older button at the top.
                <Show
                    when=move || !history_loaded.get()
                    fallback=|| ()
                >
                    <div class="feed-load-older">
                        <button
                            class="feed-load-older-btn"
                            type="button"
                            disabled=move || history_loading.get()
                            on:click=load_older
                        >
                            { move || if history_loading.get() { "Loading…" } else { "Load older" } }
                        </button>
                    </div>
                </Show>

                // Entry rows.
                <For
                    each=move || visible_rows.get()
                    key=|row| row.seq()
                    let:row
                >
                    { match row {
                        FeedRow::Single(entry) => view! {
                            <FeedEntryRow entry=entry />
                        }.into_any(),
                        FeedRow::Burst { actor_label, is_agent, count, occurred_at, channel, .. } => view! {
                            <FeedBurstRow
                                actor_label=actor_label
                                is_agent=is_agent
                                count=count
                                occurred_at=occurred_at
                                channel=channel
                            />
                        }.into_any(),
                    }}
                </For>

                // Empty state.
                <Show
                    when=move || visible_rows.with(|r| r.is_empty())
                    fallback=|| ()
                >
                    <div class="feed-empty">"No events yet."</div>
                </Show>
            </div>

            // ── "N new" chip (floats above feed list) ─────────────────────────
            <Show
                when=move || new_count.get() != 0
                fallback=|| ()
            >
                <button
                    class="feed-new-chip"
                    type="button"
                    on:click=jump_to_bottom
                >
                    { move || format!("{} new ↓", new_count.get()) }
                </button>
            </Show>
        </div>
    }
}

// ── Sub-components ────────────────────────────────────────────────────────────

/// Single-event feed row.
#[component]
fn FeedEntryRow(entry: FeedEntry) -> impl IntoView {
    let time_str = format_time(entry.occurred_at);
    let actor_class = if entry.is_agent {
        "actor-chip actor-agent"
    } else {
        "actor-chip actor-user"
    };

    view! {
        <div class="feed-entry">
            <span class="feed-entry__time">{ time_str }</span>
            <span class=actor_class>{ entry.actor_label.clone() }</span>
            <span class=channel_class(entry.channel)>{ channel_label(entry.channel) }</span>
            <span class="feed-entry__summary">{ entry.summary.clone() }</span>
            { entry.entity_id.as_ref().map(|id| view! {
                <span class="feed-entry__entity id-badge">{ id.clone() }</span>
            }) }
        </div>
    }
}

/// Burst-grouped feed row (N consecutive events from one actor).
#[component]
fn FeedBurstRow(
    actor_label: String,
    is_agent: bool,
    count: usize,
    occurred_at: Timestamp,
    channel: Channel,
) -> impl IntoView {
    let time_str = format_time(occurred_at);
    let actor_class = if is_agent {
        "actor-chip actor-agent"
    } else {
        "actor-chip actor-user"
    };

    view! {
        <div class="feed-entry feed-entry--burst">
            <span class="feed-entry__time">{ time_str }</span>
            <span class=actor_class>{ actor_label }</span>
            <span class=channel_class(channel)>{ channel_label(channel) }</span>
            <span class="feed-entry__summary feed-entry__summary--burst">
                { format!("{count} events") }
            </span>
        </div>
    }
}

/// Connection state badge (reuses `conn_state` from EventStoreCtx).
#[component]
fn ConnBadge(store: EventStoreCtx) -> impl IntoView {
    view! {
        <span class=move || conn_badge_class(&store.conn_state.get())>
            { move || conn_badge_label(&store.conn_state.get()) }
        </span>
    }
}
