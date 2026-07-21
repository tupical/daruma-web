//! Agent Operations panel — "who is working on what right now".
//!
//! Full-page live view (route `/agent-ops`), read-only, five sections:
//!
//!   1. Active agent sessions — presence: which agents are currently working.
//!   2. WorkUnits queue — what's queued and who holds each unit.
//!   3. Work leases — reserved resources with `fencing_token`, mode and TTL;
//!      expired leases are highlighted.
//!   4. Task claims — the agent→task locks that are live right now.
//!   5. AI operations in progress — folded from `Channel::AiOps` events into
//!      live progress bars (start → phase → complete).
//!
//! # Live updates
//!
//! Sections 1–4 read the VIZ-5 REST endpoints (`/v1/sessions/active`,
//! `/v1/work-units`, `/v1/leases`, `/v1/claims`). They refresh two ways: a
//! periodic 5s tick (which is also what moves a lease/claim into "expired" as
//! wall-clock passes — TTL expiry emits no event) and an immediate refetch
//! whenever a relevant WS channel event lands (`AgentStatus`, `Presence`,
//! `WorkUnits`). Section 5 is pure event-store derivation: `Channel::AiOps`
//! carries the full started/phase/completed lifecycle, so no polling is needed.

use super::fmt::{format_time, short_id, ts_millis};
use crate::api::{self, AgentClaim, AgentSession, WorkLease, WorkUnit};
use crate::event_store::{ConnState, EventStoreCtx};
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use daruma_events::{Channel, Event, EventEnvelope};
use daruma_shared::time::Timestamp;
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;

/// How often (ms) the REST-backed sections refetch and re-evaluate TTL expiry.
const POLL_INTERVAL_MS: u32 = 5_000;

fn now_ms() -> i64 {
    use chrono::Utc;
    Utc::now().timestamp_millis()
}

/// Human "time left until `expires_at`" or an "expired Ns ago" past form,
/// with a flag the caller uses to highlight the expired row.
fn ttl_display(expires_at: Timestamp) -> (String, bool) {
    let remaining_ms = ts_millis(expires_at) - now_ms();
    if remaining_ms <= 0 {
        let ago = (-remaining_ms) / 1000;
        (format!("expired {ago}s ago"), true)
    } else {
        let secs = remaining_ms / 1000;
        if secs >= 60 {
            (format!("{}m{:02}s left", secs / 60, secs % 60), false)
        } else {
            (format!("{secs}s left"), false)
        }
    }
}

// ── AI operation model ────────────────────────────────────────────────────────

/// One in-flight (or just-finished) AI operation, folded from its `AiOps`
/// event lifecycle keyed by `op_id`.
#[derive(Clone, Debug, PartialEq)]
struct AiOp {
    op_id: String,
    kind: String,
    target_id: String,
    /// Latest phase label, or "started" before the first phase change.
    phase: String,
    detail: Option<String>,
    started_at: Timestamp,
    /// `Some(outcome)` once the operation completed (`ok` or `error: …`).
    outcome: Option<String>,
}

/// Fold every `Channel::AiOps` envelope into per-op state, in first-seen order.
fn fold_ai_ops(events: &[EventEnvelope]) -> Vec<AiOp> {
    let mut order: Vec<String> = Vec::new();
    let mut ops: std::collections::HashMap<String, AiOp> = std::collections::HashMap::new();
    for env in events {
        match &env.payload {
            Event::AiOperationStarted {
                op_id,
                kind,
                target_id,
                at,
            } => {
                let key = op_id.to_string();
                if !ops.contains_key(&key) {
                    order.push(key.clone());
                }
                ops.insert(
                    key,
                    AiOp {
                        op_id: op_id.to_string(),
                        kind: kind.clone(),
                        target_id: target_id.clone(),
                        phase: "started".to_string(),
                        detail: None,
                        started_at: *at,
                        outcome: None,
                    },
                );
            }
            Event::AiOperationPhaseChanged {
                op_id,
                phase,
                detail,
                ..
            } => {
                if let Some(op) = ops.get_mut(&op_id.to_string()) {
                    op.phase = phase.clone();
                    op.detail = detail.clone();
                }
            }
            Event::AiOperationCompleted { op_id, outcome, .. } => {
                if let Some(op) = ops.get_mut(&op_id.to_string()) {
                    op.outcome = Some(outcome.clone());
                }
            }
            _ => {}
        }
    }
    order.into_iter().filter_map(|k| ops.remove(&k)).collect()
}

// ── Component ─────────────────────────────────────────────────────────────────

/// Full-screen Agent Operations dashboard. Wired into `/agent-ops` in `app.rs`.
#[component]
pub fn AgentOpsPanel() -> impl IntoView {
    let store = use_context::<EventStoreCtx>().expect("EventStoreCtx");
    let projects_ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let current_filter = projects_ctx.current_filter;

    // Project id when a single project is selected — work-units require it, and
    // claims/leases narrow to it. `None` (workspace-wide) for "all"/"inbox".
    let project_id_opt = Memo::new(move |_| match current_filter.get() {
        ProjectFilter::Of(pid) => Some(pid.to_string()),
        _ => None,
    });

    let sessions: RwSignal<Vec<AgentSession>> = RwSignal::new(Vec::new());
    let claims: RwSignal<Vec<AgentClaim>> = RwSignal::new(Vec::new());
    let leases: RwSignal<Vec<WorkLease>> = RwSignal::new(Vec::new());
    let work_units: RwSignal<Vec<WorkUnit>> = RwSignal::new(Vec::new());
    let fetch_error: RwSignal<Option<String>> = RwSignal::new(None);
    let loaded: RwSignal<bool> = RwSignal::new(false);

    // Bumped by the poll ticker and by relevant WS events; the fetch effect
    // tracks it so both paths trigger the same refetch.
    let poll_tick: RwSignal<u32> = RwSignal::new(0);
    // Cursor into `all_events` for the event-driven refetch (same idiom as
    // artifacts_panel): bump `poll_tick` when a new AgentStatus/Presence/
    // WorkUnits envelope appears.
    let applied_cursor: RwSignal<usize> = RwSignal::new(0);

    // Periodic ticker: bump `poll_tick` every POLL_INTERVAL_MS. Scoped so the
    // loop stops when the route (and this component's owner) is disposed.
    leptos::task::spawn_local_scoped_with_cancellation(async move {
        loop {
            TimeoutFuture::new(POLL_INTERVAL_MS).await;
            poll_tick.update(|n| *n = n.wrapping_add(1));
        }
    });

    // Fetch all four REST sections on mount, on project change, and on each
    // `poll_tick` bump.
    Effect::new(move |_| {
        let pid = project_id_opt.get();
        poll_tick.get(); // tracked: forces a refetch every tick

        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let mut first_err: Option<String> = None;

            match api::list_active_sessions().await {
                Ok(v) => sessions.set(v),
                Err(e) => {
                    first_err.get_or_insert_with(|| e.friendly());
                }
            };
            match api::list_active_claims(pid.as_deref()).await {
                Ok(v) => claims.set(v),
                Err(e) => {
                    first_err.get_or_insert_with(|| e.friendly());
                }
            };
            match api::list_active_leases(pid.as_deref()).await {
                Ok(v) => leases.set(v),
                Err(e) => {
                    first_err.get_or_insert_with(|| e.friendly());
                }
            };
            // Work units are project-scoped; without a project the endpoint
            // 400s, so skip the call and clear the list instead.
            match &pid {
                Some(p) => match api::list_project_work_units(p, None).await {
                    Ok(v) => work_units.set(v),
                    Err(e) => {
                        first_err.get_or_insert_with(|| e.friendly());
                    }
                },
                None => work_units.set(Vec::new()),
            };

            fetch_error.set(first_err);
            loaded.set(true);
        });
    });

    // Event-driven refetch: any new AgentStatus/Presence/WorkUnits envelope
    // bumps the tick so the REST sections refresh without waiting for the poll.
    Effect::new(move |_| {
        let len = store.all_events.with(|v| v.len());
        let start = applied_cursor.get_untracked();
        if start >= len {
            return;
        }
        let relevant = store.all_events.with_untracked(|evs| {
            evs[start..len].iter().any(|env| {
                matches!(
                    env.payload.channel(),
                    Channel::AgentStatus | Channel::Presence | Channel::WorkUnits
                )
            })
        });
        applied_cursor.set(len);
        if relevant {
            poll_tick.update(|n| *n = n.wrapping_add(1));
        }
    });

    // AI ops: derived purely from the event store's AiOps slice.
    let ai_ops = Memo::new(move |_| {
        store.all_events.with(|evs| {
            let aiops: Vec<EventEnvelope> = evs
                .iter()
                .filter(|e| e.payload.channel() == Channel::AiOps)
                .cloned()
                .collect();
            fold_ai_ops(&aiops)
        })
    });
    let ai_ops_active =
        Memo::new(move |_| ai_ops.with(|ops| ops.iter().filter(|o| o.outcome.is_none()).count()));

    view! {
        <div class="agent-ops">
            <div class="agent-ops__header">
                <span class="agent-ops__title">"Agent Operations"</span>
                <ConnBadge store=store />
                <span class="agent-ops__summary">
                    { move || format!(
                        "{} agents · {} claims · {} leases · {} AI ops",
                        sessions.with(|s| s.len()),
                        claims.with(|c| c.len()),
                        leases.with(|l| l.len()),
                        ai_ops_active.get(),
                    ) }
                </span>
            </div>

            <Show when=move || fetch_error.get().is_some() fallback=|| ()>
                <p class="fetch-error__message">
                    { move || fetch_error.get().unwrap_or_default() }
                </p>
            </Show>

            <div class="agent-ops__grid">
                <SessionsSection sessions=sessions loaded=loaded />
                <WorkUnitsSection
                    work_units=work_units
                    has_project=Signal::derive(move || project_id_opt.get().is_some())
                />
                <LeasesSection leases=leases />
                <ClaimsSection claims=claims />
                <AiOpsSection ai_ops=ai_ops />
            </div>
        </div>
    }
}

// ── Sections ──────────────────────────────────────────────────────────────────

#[component]
fn SessionsSection(sessions: RwSignal<Vec<AgentSession>>, loaded: RwSignal<bool>) -> impl IntoView {
    view! {
        <section class="ops-section">
            <div class="ops-section__head">
                <span class="ops-section__title">"Active sessions"</span>
                <span class="ops-section__count">{ move || sessions.with(|s| s.len()) }</span>
            </div>
            {move || {
                let items = sessions.get();
                if !loaded.get() {
                    view! { <div class="ops-empty">"Loading…"</div> }.into_any()
                } else if items.is_empty() {
                    view! { <div class="ops-empty">"No agents working right now."</div> }.into_any()
                } else {
                    view! {
                        <ul class="ops-list">
                            <For
                                each={ let items = items.clone(); move || items.clone() }
                                key=|s: &AgentSession| s.id.to_string()
                                let:session
                            >
                                { session_row(&session) }
                            </For>
                        </ul>
                    }.into_any()
                }
            }}
        </section>
    }
}

fn session_row(session: &AgentSession) -> AnyView {
    let agent = short_id(&session.agent_id.to_string());
    let started = format_time(session.started_at);
    let total = session.plan_steps.len();
    let in_progress = session
        .plan_steps
        .iter()
        .filter(|s| {
            use daruma_domain::SessionStepStatus;
            s.status == SessionStepStatus::InProgress
        })
        .count();
    let current = session
        .plan_steps
        .iter()
        .find(|s| {
            use daruma_domain::SessionStepStatus;
            s.status == SessionStepStatus::InProgress
        })
        .map(|s| s.content.chars().take(80).collect::<String>());
    let parent = session
        .parent_agent_id
        .as_ref()
        .map(|p| short_id(&p.to_string()));

    view! {
        <li class="ops-row ops-row--session">
            <div class="ops-row__main">
                <span class="ops-row__agent id-badge">{ agent }</span>
                { parent.map(|p| view! { <span class="ops-row__parent">{ format!("child of {p}") }</span> }) }
                <span class="ops-row__steps">
                    { if total == 0 {
                        "no plan steps".to_string()
                    } else {
                        format!("{total} steps · {in_progress} in progress")
                    } }
                </span>
                <span class="ops-row__time">{ format!("since {started}") }</span>
            </div>
            { current.map(|c| view! { <div class="ops-row__detail">{ c }</div> }) }
        </li>
    }
    .into_any()
}

#[component]
fn WorkUnitsSection(
    work_units: RwSignal<Vec<WorkUnit>>,
    has_project: Signal<bool>,
) -> impl IntoView {
    view! {
        <section class="ops-section">
            <div class="ops-section__head">
                <span class="ops-section__title">"Work units"</span>
                <span class="ops-section__count">{ move || work_units.with(|w| w.len()) }</span>
            </div>
            {move || {
                if !has_project.get() {
                    return view! {
                        <div class="ops-empty">"Select a single project to see its work-unit queue."</div>
                    }.into_any();
                }
                let items = work_units.get();
                if items.is_empty() {
                    view! { <div class="ops-empty">"No work units queued."</div> }.into_any()
                } else {
                    view! {
                        <ul class="ops-list">
                            <For
                                each={ let items = items.clone(); move || items.clone() }
                                key=|w: &WorkUnit| w.id.to_string()
                                let:unit
                            >
                                { work_unit_row(&unit) }
                            </For>
                        </ul>
                    }.into_any()
                }
            }}
        </section>
    }
}

fn work_unit_row(unit: &WorkUnit) -> AnyView {
    let status = unit.status.as_str();
    let status_class = format!("ops-status ops-status--{status}");
    let title = unit.title.chars().take(70).collect::<String>();
    let holder = unit
        .owner_agent_id
        .as_ref()
        .map(|a| short_id(&a.to_string()));
    let ttl = unit.claim_expires_at.map(ttl_display);
    let tags = unit.capability_tags.join(" ");

    view! {
        <li class="ops-row">
            <div class="ops-row__main">
                <span class=status_class>{ status.replace('_', " ") }</span>
                <span class="ops-row__title">{ title }</span>
                { match holder {
                    Some(h) => view! { <span class="ops-row__holder id-badge">{ h }</span> }.into_any(),
                    None => view! { <span class="ops-row__unheld">"unclaimed"</span> }.into_any(),
                } }
                { ttl.map(|(text, expired)| {
                    let cls = if expired { "ops-row__ttl ops-row__ttl--expired" } else { "ops-row__ttl" };
                    view! { <span class=cls>{ text }</span> }
                }) }
            </div>
            { (!tags.is_empty()).then(|| view! { <div class="ops-row__tags">{ tags }</div> }) }
        </li>
    }
    .into_any()
}

#[component]
fn LeasesSection(leases: RwSignal<Vec<WorkLease>>) -> impl IntoView {
    view! {
        <section class="ops-section">
            <div class="ops-section__head">
                <span class="ops-section__title">"Work leases"</span>
                <span class="ops-section__count">{ move || leases.with(|l| l.len()) }</span>
            </div>
            {move || {
                let items = leases.get();
                if items.is_empty() {
                    view! { <div class="ops-empty">"No active leases."</div> }.into_any()
                } else {
                    view! {
                        <ul class="ops-list">
                            <For
                                each={ let items = items.clone(); move || items.clone() }
                                key=|l: &WorkLease| l.id.to_string()
                                let:lease
                            >
                                { lease_row(&lease) }
                            </For>
                        </ul>
                    }.into_any()
                }
            }}
        </section>
    }
}

fn lease_row(lease: &WorkLease) -> AnyView {
    let agent = short_id(&lease.agent_id.to_string());
    let resource = lease
        .target_uri
        .clone()
        .unwrap_or_else(|| lease.path_glob.clone());
    let resource = resource.chars().take(60).collect::<String>();
    let mode = lease.mode.as_str();
    let fence = lease
        .fencing_token
        .map(|t| format!("#{t}"))
        .unwrap_or_else(|| "—".to_string());
    let (ttl_text, expired) = ttl_display(lease.expires_at);
    let row_class = if expired {
        "ops-row ops-row--expired"
    } else {
        "ops-row"
    };
    let ttl_class = if expired {
        "ops-row__ttl ops-row__ttl--expired"
    } else {
        "ops-row__ttl"
    };

    view! {
        <li class=row_class>
            <div class="ops-row__main">
                <span class="ops-row__agent id-badge">{ agent }</span>
                <span class=format!("ops-lease-mode ops-lease-mode--{mode}")>{ mode }</span>
                <span class="ops-row__title ops-row__resource">{ resource }</span>
                <span class="ops-row__fence" title="fencing token">{ fence }</span>
                <span class=ttl_class>{ ttl_text }</span>
            </div>
        </li>
    }
    .into_any()
}

#[component]
fn ClaimsSection(claims: RwSignal<Vec<AgentClaim>>) -> impl IntoView {
    view! {
        <section class="ops-section">
            <div class="ops-section__head">
                <span class="ops-section__title">"Task claims"</span>
                <span class="ops-section__count">{ move || claims.with(|c| c.len()) }</span>
            </div>
            {move || {
                let items = claims.get();
                if items.is_empty() {
                    view! { <div class="ops-empty">"No task claims held."</div> }.into_any()
                } else {
                    view! {
                        <ul class="ops-list">
                            <For
                                each={ let items = items.clone(); move || items.clone() }
                                key=|c: &AgentClaim| format!("{}/{}", c.agent_id, c.task_id)
                                let:claim
                            >
                                { claim_row(&claim) }
                            </For>
                        </ul>
                    }.into_any()
                }
            }}
        </section>
    }
}

fn claim_row(claim: &AgentClaim) -> AnyView {
    let agent = short_id(&claim.agent_id);
    let task = short_id(&claim.task_id);
    let (ttl_text, expired) = ttl_display(claim.expires_at);
    let ttl_class = if expired {
        "ops-row__ttl ops-row__ttl--expired"
    } else {
        "ops-row__ttl"
    };

    view! {
        <li class="ops-row">
            <div class="ops-row__main">
                <span class="ops-row__agent id-badge">{ agent }</span>
                <span class="ops-row__arrow">"holds"</span>
                <span class="ops-row__task id-badge">{ task }</span>
                <span class=ttl_class>{ ttl_text }</span>
            </div>
        </li>
    }
    .into_any()
}

#[component]
fn AiOpsSection(ai_ops: Memo<Vec<AiOp>>) -> impl IntoView {
    // Show in-progress ops as live bars; recently completed drop off once the
    // AiOps event slice no longer carries them (the store is append-only, so a
    // completed op stays visible with its outcome — useful as a short trail).
    view! {
        <section class="ops-section ops-section--wide">
            <div class="ops-section__head">
                <span class="ops-section__title">"AI operations"</span>
                <span class="ops-section__count">
                    { move || ai_ops.with(|o| o.iter().filter(|x| x.outcome.is_none()).count()) }
                </span>
            </div>
            {move || {
                let ops = ai_ops.get();
                // Newest first, in-progress ahead of completed.
                let mut ops = ops;
                ops.reverse();
                ops.sort_by_key(|o| o.outcome.is_some());
                if ops.is_empty() {
                    view! { <div class="ops-empty">"No AI operations seen yet."</div> }.into_any()
                } else {
                    view! {
                        <ul class="ops-list ops-aiops-list">
                            <For
                                each={ let ops = ops.clone(); move || ops.clone() }
                                key=|o: &AiOp| o.op_id.clone()
                                let:op
                            >
                                { ai_op_row(&op) }
                            </For>
                        </ul>
                    }.into_any()
                }
            }}
        </section>
    }
}

fn ai_op_row(op: &AiOp) -> AnyView {
    let kind = op.kind.clone();
    let target = short_id(&op.target_id);
    let started = format_time(op.started_at);
    let done = op.outcome.is_some();
    let is_error = op.outcome.as_ref().is_some_and(|o| o.starts_with("error"));

    let phase_label = match &op.outcome {
        Some(outcome) => outcome.clone(),
        None => match &op.detail {
            Some(d) => format!("{} · {}", op.phase, d.chars().take(60).collect::<String>()),
            None => op.phase.clone(),
        },
    };

    let fill_class = if is_error {
        "aiops-bar__fill aiops-bar__fill--error"
    } else if done {
        "aiops-bar__fill aiops-bar__fill--done"
    } else {
        "aiops-bar__fill aiops-bar__fill--active"
    };

    view! {
        <li class="ops-row ops-aiops-row">
            <div class="ops-row__main">
                <span class="aiops-kind">{ kind }</span>
                <span class="ops-row__task id-badge">{ target }</span>
                <span class="ops-row__time">{ format!("since {started}") }</span>
            </div>
            <div class="aiops-bar">
                <div class=fill_class></div>
            </div>
            <div class="aiops-phase">{ phase_label }</div>
        </li>
    }
    .into_any()
}

// ── Shared bits ───────────────────────────────────────────────────────────────

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

/// Connection state badge (reuses `conn_state` from EventStoreCtx), matching
/// the activity feed's badge.
#[component]
fn ConnBadge(store: EventStoreCtx) -> impl IntoView {
    view! {
        <span class=move || conn_badge_class(&store.conn_state.get())>
            { move || conn_badge_label(&store.conn_state.get()) }
        </span>
    }
}
