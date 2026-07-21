//! Time Machine — client-side replay of the workspace event log (VIZ-8).
//!
//! Loads the full event history via `GET /v1/events?since=…` (paged, same
//! endpoint the WS catch-up uses), keeps a live tail from [`EventStoreCtx`],
//! and re-projects workspace state (tasks, plans, plan membership, relations,
//! artifacts, documents, runs, projects) at any event position. Everything is
//! derived from the event log on the client; the server stays read-only and
//! no server-side snapshots are needed.
//!
//! Replay is the only mode: playback with adjustable speed, jump to seq /
//! wall-clock time, state-at-T panels plus the tail of the event log. A
//! snapshot-diff mode used to sit alongside it; it reconstructed, by
//! structurally comparing two replayed states, information the event log
//! already carries exactly — see the event strip for what happened between
//! two positions.
//!
//! # Layout
//!
//! ```text
//! ┌─ .tm ─────────────────────────────────────────────────────────────────┐
//! │ .tm-banner    "replay at seq N · time" + "not live" hint              │
//! │ .tm-controls  play/speed · slider · seq/time jump · reload            │
//! │ .tm-summary · .tm-columns (tasks/plans/links) · .tm-events            │
//! └───────────────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;

use daruma_domain::{ArtifactStatus, PlanStatus, Priority, RelationKind, RunStatus, Status};
use daruma_events::{Event, EventEnvelope};
use daruma_shared::{ArtifactId, DocumentId, PlanId, ProjectId, RelationId, RunId, TaskId};
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::fmt::{format_time, format_ts, status_class, ts_millis};
use crate::api;
use crate::components::activity_feed::{
    actor_label, channel_class, channel_label, entry_summary, is_agent_actor,
};
use crate::event_store::EventStoreCtx;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Events fetched per page while loading history (server caps at 500).
const PAGE_SIZE: usize = 500;

/// Playback tick.
const TICK_MS: u32 = 100;

/// Playback speeds: (label, events advanced per tick).
const SPEEDS: &[(&str, usize)] = &[("1×", 1), ("4×", 4), ("16×", 16), ("64×", 64)];

/// Events shown in the replay event strip (the tail ending at the cursor).
const EVENT_WINDOW: usize = 40;

/// Per-group item cap in the state panels; overflow folds into "+N more".
const GROUP_CAP: usize = 30;

/// Task status display order in the replay board.
const STATUS_ORDER: &[Status] = &[
    Status::InProgress,
    Status::InReview,
    Status::Todo,
    Status::Inbox,
    Status::Done,
    Status::Cancelled,
];

// ── Reprojection ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
struct TmTask {
    title: String,
    status: Status,
    priority: Priority,
    project_id: Option<ProjectId>,
    created_seq: u64,
    last_seq: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct TmPlan {
    title: String,
    status: PlanStatus,
    archived: bool,
    project_id: ProjectId,
    created_seq: u64,
    last_seq: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct TmRelation {
    from: TaskId,
    to: TaskId,
    kind: RelationKind,
}

#[derive(Clone, Debug, PartialEq)]
struct TmArtifact {
    title: String,
    status: ArtifactStatus,
    created_seq: u64,
    last_seq: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct TmDoc {
    title: String,
    archived: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct TmRun {
    plan_id: PlanId,
    status: RunStatus,
    last_seq: u64,
}

/// Workspace state re-projected from a prefix of the event log.
/// `*_order` vectors keep creation order for stable rendering.
#[derive(Clone, Default, PartialEq)]
struct TmState {
    tasks: HashMap<TaskId, TmTask>,
    task_order: Vec<TaskId>,
    plans: HashMap<PlanId, TmPlan>,
    plan_order: Vec<PlanId>,
    plan_tasks: HashMap<PlanId, Vec<TaskId>>,
    relations: HashMap<RelationId, TmRelation>,
    relation_order: Vec<RelationId>,
    artifacts: HashMap<ArtifactId, TmArtifact>,
    artifact_order: Vec<ArtifactId>,
    docs: HashMap<DocumentId, TmDoc>,
    runs: HashMap<RunId, TmRun>,
    run_order: Vec<RunId>,
    /// Project id → title, for display next to tasks/plans.
    projects: HashMap<ProjectId, String>,
}

fn apply_event(state: &mut TmState, env: &EventEnvelope) {
    let seq = env.seq;
    match &env.payload {
        Event::ProjectCreated { project } => {
            state.projects.insert(project.id, project.title.clone());
        }
        Event::ProjectUpdated {
            project_id, title, ..
        } => {
            if let Some(title) = title {
                state.projects.insert(*project_id, title.clone());
            }
        }
        Event::ProjectDeleted { project_id } => {
            state.projects.remove(project_id);
        }
        Event::TaskCreated { task } => {
            // The server fills `id` before emitting (handler.rs), so `None`
            // only shows up in hand-written fixtures — those are skipped.
            if let Some(id) = task.id {
                state.task_order.push(id);
                state.tasks.insert(
                    id,
                    TmTask {
                        title: task.title.clone(),
                        status: task.status.unwrap_or_default(),
                        priority: task.priority.unwrap_or_default(),
                        project_id: task.project_id,
                        created_seq: seq,
                        last_seq: seq,
                    },
                );
            }
        }
        Event::TaskSplitGenerated { subtasks, .. } => {
            for task in subtasks {
                if let Some(id) = task.id {
                    state.task_order.push(id);
                    state.tasks.insert(
                        id,
                        TmTask {
                            title: task.title.clone(),
                            status: task.status.unwrap_or_default(),
                            priority: task.priority.unwrap_or_default(),
                            project_id: task.project_id,
                            created_seq: seq,
                            last_seq: seq,
                        },
                    );
                }
            }
        }
        Event::TaskUpdated { task_id, patch } => {
            if let Some(t) = state.tasks.get_mut(task_id) {
                if let Some(title) = &patch.title {
                    t.title = title.clone();
                }
                if let Some(status) = &patch.status {
                    t.status = *status;
                }
                if let Some(priority) = &patch.priority {
                    t.priority = *priority;
                }
                if let Some(project_id) = &patch.project_id {
                    t.project_id = *project_id;
                }
                t.last_seq = seq;
            }
        }
        Event::TaskStatusChanged { task_id, to, .. } => {
            if let Some(t) = state.tasks.get_mut(task_id) {
                t.status = *to;
                t.last_seq = seq;
            }
        }
        Event::TaskPriorityChanged { task_id, to, .. } => {
            if let Some(t) = state.tasks.get_mut(task_id) {
                t.priority = *to;
                t.last_seq = seq;
            }
        }
        Event::TaskCompleted { task_id, .. } => {
            if let Some(t) = state.tasks.get_mut(task_id) {
                t.status = Status::Done;
                t.last_seq = seq;
            }
        }
        Event::TaskDeleted { task_id } => {
            state.tasks.remove(task_id);
            state.task_order.retain(|id| id != task_id);
        }
        Event::TaskLinked {
            relation_id,
            from,
            to,
            kind,
            ..
        } => {
            state.relation_order.push(*relation_id);
            state.relations.insert(
                *relation_id,
                TmRelation {
                    from: *from,
                    to: *to,
                    kind: *kind,
                },
            );
        }
        Event::TaskUnlinked { relation_id, .. } => {
            state.relations.remove(relation_id);
            state.relation_order.retain(|id| id != relation_id);
        }
        Event::TaskRelationKindChanged {
            relation_id,
            to_kind,
            ..
        } => {
            if let Some(r) = state.relations.get_mut(relation_id) {
                r.kind = *to_kind;
            }
        }
        Event::PlanCreated { plan } => {
            state.plan_order.push(plan.id);
            state.plans.insert(
                plan.id,
                TmPlan {
                    title: plan.title.clone(),
                    status: plan.status,
                    archived: false,
                    project_id: plan.project_id,
                    created_seq: seq,
                    last_seq: seq,
                },
            );
        }
        Event::PlanUpdated { plan_id, patch } => {
            if let Some(p) = state.plans.get_mut(plan_id) {
                if let Some(title) = &patch.title {
                    p.title = title.clone();
                }
                p.last_seq = seq;
            }
        }
        Event::PlanStatusChanged { plan_id, to, .. } => {
            if let Some(p) = state.plans.get_mut(plan_id) {
                p.status = *to;
                p.last_seq = seq;
            }
        }
        Event::PlanArchived { plan_id, .. } => {
            if let Some(p) = state.plans.get_mut(plan_id) {
                p.archived = true;
                p.last_seq = seq;
            }
        }
        Event::PlanTaskAdded {
            plan_id, task_id, ..
        } => {
            let list = state.plan_tasks.entry(*plan_id).or_default();
            if !list.contains(task_id) {
                list.push(*task_id);
            }
        }
        Event::PlanTaskRemoved { plan_id, task_id } => {
            if let Some(list) = state.plan_tasks.get_mut(plan_id) {
                list.retain(|id| id != task_id);
            }
        }
        Event::RunStarted { run } => {
            state.run_order.push(run.id);
            state.runs.insert(
                run.id,
                TmRun {
                    plan_id: run.plan_id,
                    status: run.status,
                    last_seq: seq,
                },
            );
        }
        Event::RunCompleted { run_id, .. } => {
            if let Some(r) = state.runs.get_mut(run_id) {
                r.status = RunStatus::Completed;
                r.last_seq = seq;
            }
        }
        Event::RunFailed { run_id, .. } => {
            if let Some(r) = state.runs.get_mut(run_id) {
                r.status = RunStatus::Failed;
                r.last_seq = seq;
            }
        }
        Event::RunAborted { run_id, .. } => {
            if let Some(r) = state.runs.get_mut(run_id) {
                r.status = RunStatus::Aborted;
                r.last_seq = seq;
            }
        }
        Event::ArtifactRegistered { artifact } => {
            state.artifact_order.push(artifact.id);
            state.artifacts.insert(
                artifact.id,
                TmArtifact {
                    title: artifact.title.clone(),
                    status: artifact.status,
                    created_seq: seq,
                    last_seq: seq,
                },
            );
        }
        Event::ArtifactStatusChanged {
            artifact_id, to, ..
        } => {
            if let Some(a) = state.artifacts.get_mut(artifact_id) {
                a.status = *to;
                a.last_seq = seq;
            }
        }
        Event::ArtifactChanged {
            artifact_id, title, ..
        } => {
            if let Some(a) = state.artifacts.get_mut(artifact_id) {
                if let Some(title) = title {
                    a.title = title.clone();
                }
                a.last_seq = seq;
            }
        }
        Event::ArtifactDeprecated { artifact_id, .. } => {
            if let Some(a) = state.artifacts.get_mut(artifact_id) {
                a.status = ArtifactStatus::Deprecated;
                a.last_seq = seq;
            }
        }
        Event::DocumentCreated { document } => {
            state.docs.insert(
                document.id,
                TmDoc {
                    title: document.title.clone(),
                    archived: false,
                },
            );
        }
        Event::DocumentRenamed {
            document_id, title, ..
        } => {
            if let Some(d) = state.docs.get_mut(document_id) {
                d.title = title.clone();
            }
        }
        Event::DocumentArchived { document_id, .. } => {
            if let Some(d) = state.docs.get_mut(document_id) {
                d.archived = true;
            }
        }
        _ => {}
    }
}

/// Re-project the workspace state after applying the first `count` events.
fn replay(events: &[EventEnvelope], count: usize) -> TmState {
    let mut state = TmState::default();
    for env in events.iter().take(count) {
        apply_event(&mut state, env);
    }
    state
}

// ── Small helpers ─────────────────────────────────────────────────────────────

fn status_label(s: Status) -> &'static str {
    s.as_str()
}

fn short_id<T: std::fmt::Display>(id: T) -> String {
    id.to_string().chars().take(13).collect()
}

/// Page through `GET /v1/events?since=…` until the head and store the full
/// history in `base`. `pos` snaps to the head on the first load.
fn load_history(
    base: RwSignal<Vec<EventEnvelope>>,
    base_max_seq: RwSignal<u64>,
    loading: RwSignal<bool>,
    load_error: RwSignal<Option<String>>,
    pos: RwSignal<usize>,
    first_load: RwSignal<bool>,
) {
    loading.set(true);
    load_error.set(None);
    spawn_local(async move {
        let mut cursor = 0u64;
        let mut all: Vec<EventEnvelope> = Vec::new();
        loop {
            match api::events_since(cursor, PAGE_SIZE).await {
                Ok(page) => {
                    let done = page.len() < PAGE_SIZE;
                    if let Some(last) = page.last() {
                        cursor = last.seq;
                    }
                    all.extend(page);
                    if done {
                        break;
                    }
                }
                Err(e) => {
                    load_error.set(Some(e.friendly()));
                    break;
                }
            }
        }
        all.sort_by_key(|e| e.seq);
        base_max_seq.set(all.last().map(|e| e.seq).unwrap_or(0));
        let len = all.len();
        base.set(all);
        if first_load.get_untracked() {
            pos.set(len);
            first_load.set(false);
        }
        loading.set(false);
    });
}

// ── Component ─────────────────────────────────────────────────────────────────

/// Full-screen Time Machine view; mounted on the `/time-machine` route.
#[component]
pub fn TimeMachine() -> impl IntoView {
    let store = use_context::<EventStoreCtx>().expect("EventStoreCtx");

    // ── History loading ───────────────────────────────────────────────────
    let base: RwSignal<Vec<EventEnvelope>> = RwSignal::new(Vec::new());
    let base_max_seq: RwSignal<u64> = RwSignal::new(0);
    let loading: RwSignal<bool> = RwSignal::new(true);
    let load_error: RwSignal<Option<String>> = RwSignal::new(None);
    let first_load: RwSignal<bool> = RwSignal::new(true);

    // ── Cursor + mode ─────────────────────────────────────────────────────
    // Number of leading events applied (0 = before the first event).
    let pos: RwSignal<usize> = RwSignal::new(0);
    let playing: RwSignal<bool> = RwSignal::new(false);
    let speed_idx: RwSignal<usize> = RwSignal::new(1);

    load_history(base, base_max_seq, loading, load_error, pos, first_load);

    // Full log = fetched history + live tail (deduped by seq).
    let events = Memo::new(move |_| {
        let mut v = base.get();
        let max = base_max_seq.get();
        v.extend(store.all_events.get().into_iter().filter(|e| e.seq > max));
        v
    });
    let events_len = Memo::new(move |_| events.with(|v| v.len()));

    // Clamp the cursors if the log shrinks below them (never happens with an
    // append-only log, but keeps indexing panic-free on refetch).
    Effect::new(move |_| {
        let len = events_len.get();
        if pos.get_untracked() > len {
            pos.set(len);
        }
    });

    // ── Playback engine ───────────────────────────────────────────────────
    // Scoped tick loop (same pattern as status_bar/agent_ops polling): the
    // effect re-runs on play/pause/speed, disposing the previous run's task.
    Effect::new(move |_| {
        if !playing.get() {
            return;
        }
        let step = SPEEDS[speed_idx.get()].1;
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            loop {
                TimeoutFuture::new(TICK_MS).await;
                let len = events_len.get_untracked();
                let p = pos.get_untracked();
                if p + step >= len {
                    pos.set(len);
                    playing.set(false);
                    break;
                }
                pos.set(p + step);
            }
        });
    });

    // ── Derived state ─────────────────────────────────────────────────────
    let current = Memo::new(move |_| {
        let p = pos.get();
        if p == 0 {
            return None;
        }
        events.with(|v| v.get(p - 1).cloned())
    });

    let state = Memo::new(move |_| {
        let p = pos.get();
        events.with(|v| replay(v, p.min(v.len())))
    });

    // ── Control handlers ──────────────────────────────────────────────────
    let play_toggle = move |_| {
        if playing.get_untracked() {
            playing.set(false);
        } else {
            if pos.get_untracked() >= events_len.get_untracked() {
                pos.set(0);
            }
            playing.set(true);
        }
    };

    let on_slider = move |ev| {
        if let Ok(p) = event_target_value(&ev).parse::<usize>() {
            pos.set(p);
            playing.set(false);
        }
    };

    let jump_seq = move |ev| {
        if let Ok(target) = event_target_value(&ev).trim().parse::<u64>() {
            let p = events.with_untracked(|v| v.partition_point(|e| e.seq <= target));
            pos.set(p);
            playing.set(false);
        }
    };

    let jump_time = move |ev| {
        let raw = event_target_value(&ev);
        let ms = js_sys::Date::parse(&raw);
        if raw.is_empty() || ms.is_nan() {
            return;
        }
        let target = ms as i64;
        let p =
            events.with_untracked(|v| v.partition_point(|e| ts_millis(e.occurred_at) <= target));
        pos.set(p);
        playing.set(false);
    };

    let step_by = move |delta: isize| {
        let len = events_len.get_untracked();
        let p = pos.get_untracked() as isize + delta;
        pos.set(p.clamp(0, len as isize) as usize);
        playing.set(false);
    };

    let cur_seq = Memo::new(move |_| current.get().map(|e| e.seq));

    // ── View ──────────────────────────────────────────────────────────────
    view! {
        <div class="tm">
            // ── Banner: makes the "not live" state unmistakable ────────────
            <div class="tm-banner">
                <span class="tm-banner__badge">"⏪ Time Machine"</span>
                <span class="tm-banner__pos">
                    {move || match current.get() {
                        Some(env) => format!(
                            "replay at seq {} · {} UTC",
                            env.seq,
                            format_ts(env.occurred_at)
                        ),
                        None => "before the first event".to_string(),
                    }}
                </span>
                <span class="tm-banner__hint">"historical replay — not live"</span>
            </div>

            // ── Controls ──────────────────────────────────────────────────
            <div class="tm-controls">
                <button class="tm-btn" type="button" title="Play / pause playback"
                    on:click=play_toggle>
                    {move || if playing.get() { "⏸" } else { "▶" }}
                </button>
                { SPEEDS.iter().enumerate().map(|(i, (label, _))| view! {
                    <button
                        class=move || if speed_idx.get() == i {
                            "tm-btn tm-btn--active"
                        } else {
                            "tm-btn"
                        }
                        type="button"
                        title="Playback speed"
                        on:click=move |_| speed_idx.set(i)
                    >
                        { *label }
                    </button>
                }).collect_view() }
                <span class="tm-controls__sep"></span>
                <button class="tm-btn" type="button" title="Jump to start"
                    on:click=move |_| { pos.set(0); playing.set(false); }>
                    "⏮"
                </button>
                <button class="tm-btn" type="button" title="One event back"
                    on:click=move |_| step_by(-1)>
                    "‹"
                </button>
                <button class="tm-btn" type="button" title="One event forward"
                    on:click=move |_| step_by(1)>
                    "›"
                </button>
                <button class="tm-btn" type="button" title="Jump to now"
                    on:click=move |_| { pos.set(events_len.get_untracked()); playing.set(false); }>
                    "⏭"
                </button>
                <input
                    type="range"
                    class="tm-slider"
                    min="0"
                    max=move || events_len.get().to_string()
                    prop:value=move || pos.get().to_string()
                    on:input=on_slider
                />
                <input
                    type="number"
                    class="tm-input tm-input--seq"
                    placeholder="seq"
                    min="0"
                    title="Jump to event seq"
                    on:change=jump_seq
                />
                <input
                    type="datetime-local"
                    class="tm-input"
                    title="Jump to wall-clock time (local)"
                    on:change=jump_time
                />
                <span class="tm-pos">{move || format!("{}/{}", pos.get(), events_len.get())}</span>
                <button class="tm-btn" type="button" title="Reload history"
                    on:click=move |_| load_history(
                        base, base_max_seq, loading, load_error, pos, first_load,
                    )>
                    "↻"
                </button>
            </div>

            // ── Loading / error ───────────────────────────────────────────
            <Show when=move || loading.get() fallback=|| ()>
                <div class="tm-note">"Loading event history…"</div>
            </Show>
            <Show when=move || load_error.get().is_some() fallback=|| ()>
                <p class="fetch-error__message">
                    {move || load_error.get().unwrap_or_default()}
                </p>
            </Show>
            <Show when=move || !loading.get() && events_len.get() == 0 fallback=|| ()>
                <div class="tm-note">"No events yet — the workspace log is empty."</div>
            </Show>

            <ReplayView state=state cur_seq=cur_seq events=events pos=pos />
        </div>
    }
}

// ── Replay view ───────────────────────────────────────────────────────────────

#[component]
fn ReplayView(
    state: Memo<TmState>,
    cur_seq: Memo<Option<u64>>,
    events: Memo<Vec<EventEnvelope>>,
    pos: RwSignal<usize>,
) -> impl IntoView {
    view! {
        // ── Summary chips ─────────────────────────────────────────────────
        <div class="tm-summary">
            {move || state.with(|s| {
                let mut chips: Vec<String> = Vec::new();
                for st in STATUS_ORDER {
                    let n = s.tasks.values().filter(|t| t.status == *st).count();
                    if n > 0 {
                        chips.push(format!("{} ×{}", status_label(*st), n));
                    }
                }
                chips.push(format!("plans ×{}", s.plans.len()));
                let active_runs = s.runs.values().filter(|r| r.status == RunStatus::Active).count();
                if active_runs > 0 {
                    chips.push(format!("runs active ×{}", active_runs));
                }
                chips.push(format!("artifacts ×{}", s.artifacts.len()));
                chips.push(format!("links ×{}", s.relations.len()));
                chips.push(format!("docs ×{}", s.docs.len()));
                chips.into_iter().map(|c| view! { <span class="tm-chip">{c}</span> })
                    .collect_view()
            })}
        </div>

        // ── State panels ──────────────────────────────────────────────────
        <div class="tm-columns">
            <TasksPanel state=state cur_seq=cur_seq />
            <PlansPanel state=state cur_seq=cur_seq />
            <LinksPanel state=state cur_seq=cur_seq />
        </div>

        // ── Event strip: the log tail ending at the cursor ────────────────
        <div class="tm-events">
            <div class="tm-panel__title">"Events up to cursor"</div>
            {move || {
                let p = pos.get();
                let cur = cur_seq.get();
                events.with(|v| {
                    let p = p.min(v.len());
                    let start = p.saturating_sub(EVENT_WINDOW);
                    v[start..p]
                        .iter()
                        .map(|env| {
                            let (summary, _) = entry_summary(env);
                            let is_cur = Some(env.seq) == cur;
                            view! {
                                <div class=if is_cur {
                                    "feed-entry feed-entry--tm-current"
                                } else {
                                    "feed-entry"
                                }>
                                    <span class="feed-entry__time">
                                        {format!("{} · {}", format_time(env.occurred_at), env.seq)}
                                    </span>
                                    <span class=if is_agent_actor(env) {
                                        "actor-chip actor-agent"
                                    } else {
                                        "actor-chip actor-user"
                                    }>{actor_label(env)}</span>
                                    <span class=channel_class(env.payload.channel())>
                                        {channel_label(env.payload.channel())}
                                    </span>
                                    <span class="feed-entry__summary">{summary}</span>
                                </div>
                            }
                        })
                        .collect_view()
                })
            }}
        </div>
    }
}

// ── Replay panels ─────────────────────────────────────────────────────────────

#[component]
fn TasksPanel(state: Memo<TmState>, cur_seq: Memo<Option<u64>>) -> impl IntoView {
    view! {
        <div class="tm-panel">
            <div class="tm-panel__title">"Tasks"</div>
            {move || state.with(|s| {
                let cur = cur_seq.get();
                STATUS_ORDER
                    .iter()
                    .filter_map(|st| {
                        let tasks: Vec<(TaskId, TmTask)> = s
                            .task_order
                            .iter()
                            .filter_map(|id| s.tasks.get(id).map(|t| (*id, t.clone())))
                            .filter(|(_, t)| t.status == *st)
                            .collect();
                        if tasks.is_empty() {
                            return None;
                        }
                        let total = tasks.len();
                        let rows = tasks
                            .into_iter()
                            .take(GROUP_CAP)
                            .map(|(_, t)| {
                                let hot = t.last_seq == cur.unwrap_or(0);
                                let project = t
                                    .project_id
                                    .and_then(|pid| s.projects.get(&pid).cloned())
                                    .unwrap_or_else(|| "inbox".to_string());
                                let title = t.title.clone();
                                view! {
                                    <div class=if hot { "tm-card tm-card--hot" } else { "tm-card" }>
                                        <span class=status_class(t.status)>{status_label(t.status)}</span>
                                        <span class="tm-card__title" title=title.clone()>{title.clone()}</span>
                                        <span class="tm-card__meta">
                                            {format!("{} · {}", t.priority.as_str(), project)}
                                        </span>
                                    </div>
                                }
                            })
                            .collect_view();
                        let overflow = (total > GROUP_CAP).then(|| view! {
                            <div class="tm-card__meta tm-more">{format!("… +{} more", total - GROUP_CAP)}</div>
                        });
                        Some(view! {
                            <div class="tm-group">
                                <div class="tm-group__header">
                                    {format!("{} ({})", status_label(*st), total)}
                                </div>
                                {rows}
                                {overflow}
                            </div>
                        })
                    })
                    .collect_view()
            })}
        </div>
    }
}

#[component]
fn PlansPanel(state: Memo<TmState>, cur_seq: Memo<Option<u64>>) -> impl IntoView {
    view! {
        <div class="tm-panel">
            <div class="tm-panel__title">"Plans & runs"</div>
            {move || state.with(|s| {
                let cur = cur_seq.get();
                let plans = s
                    .plan_order
                    .iter()
                    .filter_map(|id| s.plans.get(id).map(|p| (*id, p.clone())))
                    .map(|(id, p)| {
                        let hot = p.last_seq == cur.unwrap_or(0);
                        let tasks = s.plan_tasks.get(&id).cloned().unwrap_or_default();
                        let done = tasks
                            .iter()
                            .filter(|tid| {
                                s.tasks.get(tid).map(|t| t.status == Status::Done).unwrap_or(false)
                            })
                            .count();
                        let project = s
                            .projects
                            .get(&p.project_id)
                            .cloned()
                            .unwrap_or_default();
                        let status = if p.archived {
                            "archived".to_string()
                        } else {
                            format!("{:?}", p.status).to_lowercase()
                        };
                        let title = p.title.clone();
                        view! {
                            <div class=if hot { "tm-card tm-card--hot" } else { "tm-card" }>
                                <span class="tm-card__title" title=title.clone()>{title.clone()}</span>
                                <span class="tm-card__meta">
                                    {format!("{} · {}/{} · {}", status, done, tasks.len(), project)}
                                </span>
                            </div>
                        }
                    })
                    .collect_view();
                let active_runs: Vec<_> = s
                    .run_order
                    .iter()
                    .filter_map(|id| s.runs.get(id).map(|r| (*id, r.clone())))
                    .filter(|(_, r)| r.status == RunStatus::Active)
                    .collect();
                let has_runs = !active_runs.is_empty();
                let runs = active_runs
                    .into_iter()
                    .map(|(id, r)| {
                        let hot = r.last_seq == cur.unwrap_or(0);
                        let plan = s
                            .plans
                            .get(&r.plan_id)
                            .map(|p| p.title.clone())
                            .unwrap_or_else(|| short_id(r.plan_id));
                        view! {
                            <div class=if hot { "tm-card tm-card--hot" } else { "tm-card" }>
                                <span class="tm-card__title">{format!("run {}", short_id(id))}</span>
                                <span class="tm-card__meta">{format!("active · {}", plan)}</span>
                            </div>
                        }
                    })
                    .collect_view();
                view! {
                    {plans}
                    {has_runs.then(|| view! { <div class="tm-group__header">"active runs"</div> })}
                    {runs}
                }
            })}
        </div>
    }
}

#[component]
fn LinksPanel(state: Memo<TmState>, cur_seq: Memo<Option<u64>>) -> impl IntoView {
    let title_of = move |s: &TmState, id: TaskId| {
        s.tasks
            .get(&id)
            .map(|t| t.title.clone())
            .unwrap_or_else(|| short_id(id))
    };
    view! {
        <div class="tm-panel">
            <div class="tm-panel__title">"Artifacts & links"</div>
            {move || state.with(|s| {
                let cur = cur_seq.get();
                let artifacts_total = s.artifacts.len();
                let artifacts = s
                    .artifact_order
                    .iter()
                    .filter_map(|id| s.artifacts.get(id).map(|a| (*id, a.clone())))
                    .take(GROUP_CAP)
                    .map(|(_, a)| {
                        let hot = a.last_seq == cur.unwrap_or(0);
                        let title = a.title.clone();
                        view! {
                            <div class=if hot { "tm-card tm-card--hot" } else { "tm-card" }>
                                <span class="tm-card__title" title=title.clone()>{title.clone()}</span>
                                <span class="tm-card__meta">{format!("{:?}", a.status).to_lowercase()}</span>
                            </div>
                        }
                    })
                    .collect_view();
                let artifacts_overflow = (artifacts_total > GROUP_CAP).then(|| view! {
                    <div class="tm-card__meta tm-more">
                        {format!("… +{} more", artifacts_total - GROUP_CAP)}
                    </div>
                });
                let relations_total = s.relations.len();
                let relations = s
                    .relation_order
                    .iter()
                    .filter_map(|id| s.relations.get(id).map(|r| (*id, r.clone())))
                    .take(GROUP_CAP)
                    .map(|(_, r)| {
                        view! {
                            <div class="tm-card">
                                <span class="tm-card__title">
                                    {format!(
                                        "{} → {}",
                                        title_of(s, r.from),
                                        title_of(s, r.to)
                                    )}
                                </span>
                                <span class="tm-card__meta">{format!("{:?}", r.kind)}</span>
                            </div>
                        }
                    })
                    .collect_view();
                let relations_overflow = (relations_total > GROUP_CAP).then(|| view! {
                    <div class="tm-card__meta tm-more">
                        {format!("… +{} more", relations_total - GROUP_CAP)}
                    </div>
                });
                view! {
                    <div class="tm-group__header">{format!("artifacts ({})", artifacts_total)}</div>
                    {artifacts}
                    {artifacts_overflow}
                    <div class="tm-group__header">{format!("links ({})", relations_total)}</div>
                    {relations}
                    {relations_overflow}
                }
            })}
        </div>
    }
}
