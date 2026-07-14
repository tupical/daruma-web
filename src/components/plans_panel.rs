use crate::api;
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::ws::WsCtx;
use leptos::prelude::*;
use std::collections::{HashMap, HashSet};
use daruma_domain::{Plan, PlanPatch, PlanStatus, Status};
use daruma_events::{Channel, Event, EventEnvelope};
use daruma_shared::time::Timestamp;
use daruma_shared::TaskId;

const PLAN_GROUP_ORDER: &[PlanStatus] = &[
    PlanStatus::Active,
    PlanStatus::Draft,
    PlanStatus::Completed,
    PlanStatus::Abandoned,
];

fn status_class(status: &PlanStatus) -> &'static str {
    match status {
        PlanStatus::Draft => "plan-status plan-status-draft",
        PlanStatus::Active => "plan-status plan-status-active",
        PlanStatus::Completed => "plan-status plan-status-completed",
        PlanStatus::Abandoned => "plan-status plan-status-abandoned",
    }
}

fn apply_plan_patch(patch: &PlanPatch, plan: &mut Plan, at: Timestamp) {
    if let Some(t) = &patch.title {
        plan.title = t.clone();
    }
    if let Some(d) = &patch.description {
        plan.description = d.clone();
    }
    if let Some(g) = &patch.goal {
        plan.goal = g.clone();
    }
    if let Some(sc) = &patch.success_criteria {
        plan.success_criteria = sc.clone();
    }
    if let Some(p) = &patch.parent_plan_id {
        plan.parent_plan_id = p.clone();
    }
    plan.updated_at = at;
}

fn status_label(status: &PlanStatus) -> &'static str {
    match status {
        PlanStatus::Draft => "draft",
        PlanStatus::Active => "active",
        PlanStatus::Completed => "completed",
        PlanStatus::Abandoned => "abandoned",
    }
}

fn plan_group_label(s: PlanStatus) -> &'static str {
    match s {
        PlanStatus::Active => "Active",
        PlanStatus::Draft => "Draft",
        PlanStatus::Completed => "Completed",
        PlanStatus::Abandoned => "Abandoned",
    }
}

fn plan_group_slug(s: PlanStatus) -> &'static str {
    match s {
        PlanStatus::Active => "active",
        PlanStatus::Draft => "draft",
        PlanStatus::Completed => "completed",
        PlanStatus::Abandoned => "abandoned",
    }
}

/// Apply one WS event to a single per-project plan list. Idempotent by id.
fn apply_plan_event(env: &EventEnvelope, list: &mut Vec<Plan>, project_id: &str) {
    match &env.payload {
        Event::PlanCreated { plan } => {
            if plan.project_id.to_string() != project_id {
                return;
            }
            if !list.iter().any(|p| p.id == plan.id) {
                list.push(plan.clone());
            }
        }
        Event::PlanUpdated { plan_id, patch } => {
            if let Some(p) = list.iter_mut().find(|p| p.id == *plan_id) {
                apply_plan_patch(patch, p, env.occurred_at);
            }
        }
        Event::PlanStatusChanged { plan_id, to, .. } => {
            if let Some(p) = list.iter_mut().find(|p| p.id == *plan_id) {
                p.status = *to;
            }
        }
        Event::PlanGoalChanged { plan_id, to, .. } => {
            if let Some(p) = list.iter_mut().find(|p| p.id == *plan_id) {
                p.goal = to.clone();
            }
        }
        Event::PlanArchived { plan_id, at } => {
            if let Some(p) = list.iter_mut().find(|p| p.id == *plan_id) {
                p.archived_at = Some(*at);
            }
        }
        // PlanTaskAdded / PlanTaskRemoved / PlanReordered touch the task list
        // inside a plan; this panel only renders the plan header, so they
        // don't change anything we display.
        _ => {}
    }
}

// ── Tree data structure ───────────────────────────────────────────────────────

struct PlanTreeNode {
    plan: Plan,
    children: Vec<PlanTreeNode>,
}

/// Build a forest from a flat plan list, linking children by `parent_plan_id`.
/// Root nodes are those with no parent.
fn build_tree(plans: Vec<Plan>) -> Vec<PlanTreeNode> {
    let roots: Vec<Plan> = plans
        .iter()
        .filter(|p| p.parent_plan_id.is_none())
        .cloned()
        .collect();
    roots
        .into_iter()
        .map(|p| build_subtree(p, &plans))
        .collect()
}

fn build_subtree(plan: Plan, all: &[Plan]) -> PlanTreeNode {
    let id = plan.id;
    let children = all
        .iter()
        .filter(|p| p.parent_plan_id == Some(id))
        .cloned()
        .map(|child| build_subtree(child, all))
        .collect();
    PlanTreeNode { plan, children }
}

fn group_roots_by_status(roots: Vec<PlanTreeNode>) -> Vec<(PlanStatus, Vec<PlanTreeNode>)> {
    let mut buckets: [Vec<PlanTreeNode>; 4] = Default::default();
    for node in roots {
        if let Some(idx) = PLAN_GROUP_ORDER
            .iter()
            .position(|&status| status == node.plan.status)
        {
            buckets[idx].push(node);
        }
    }
    PLAN_GROUP_ORDER
        .iter()
        .copied()
        .zip(buckets)
        .filter_map(|(status, nodes)| {
            if nodes.is_empty() {
                None
            } else {
                Some((status, nodes))
            }
        })
        .collect()
}

// ── Plan dependency graph (VIZ-6, plan half) ────────────────────────────────
//
// Lazy per-plan subpanel: `GET /plans/{id}/graph` (task DAG) + `/fanout`
// (execution waves) + `/progress` (counts), rendered as plain lists/levels —
// no graph-visualization library, per spec. Critical path is computed
// client-side from the graph (cheap: plan task counts are small, and it's
// only computed once per fetch, not per render).

/// Task-status pill, reusing exactly the class names task_list.rs/task_row.rs
/// use for `Task.status` — `PlanGraphNode.status` is the same `Status` enum,
/// so the existing `.status-*` colors apply with no new CSS.
fn task_status_class(status: Status) -> &'static str {
    match status {
        Status::Inbox => "status status-inbox",
        Status::Todo => "status status-todo",
        Status::InProgress => "status status-in-progress",
        Status::InReview => "status status-in-review",
        Status::Done => "status status-done",
        Status::Cancelled => "status status-cancelled",
    }
}

fn task_status_label(status: Status) -> &'static str {
    match status {
        Status::Inbox => "Inbox",
        Status::Todo => "Todo",
        Status::InProgress => "In Progress",
        Status::InReview => "In Review",
        Status::Done => "Done",
        Status::Cancelled => "Cancelled",
    }
}

/// Last 8 non-hyphen characters of an id string (same convention as
/// `task_row.rs`'s `short_id` / `artifacts_panel.rs`'s copy of it).
fn short_id(id: &str) -> String {
    let compact: String = id.chars().filter(|&c| c != '-').collect();
    if compact.len() >= 8 {
        compact[compact.len() - 8..].to_string()
    } else {
        compact
    }
}

/// Longest dependency chain through the graph (unweighted — number of hops),
/// as a set for O(1) "is this task on the critical path" lookups. `edges`
/// point blocker -> blocked (both `depends_on` and `blocks` share that
/// direction, per `plan_readiness::plan_graph`'s construction server-side),
/// so this is a standard DAG longest-path via memoized DFS. A `visiting`
/// guard makes it safe against a malformed cyclic input instead of
/// stack-overflowing the tab.
fn critical_path(nodes: &[api::PlanGraphNode], edges: &[api::PlanGraphEdge]) -> HashSet<TaskId> {
    let node_ids: HashSet<TaskId> = nodes.iter().map(|n| n.task_id).collect();
    let mut preds: HashMap<TaskId, Vec<TaskId>> = HashMap::new();
    for e in edges {
        if node_ids.contains(&e.from) && node_ids.contains(&e.to) {
            preds.entry(e.to).or_default().push(e.from);
        }
    }

    fn longest_to(
        id: TaskId,
        preds: &HashMap<TaskId, Vec<TaskId>>,
        memo: &mut HashMap<TaskId, usize>,
        visiting: &mut HashSet<TaskId>,
    ) -> usize {
        if let Some(&cached) = memo.get(&id) {
            return cached;
        }
        if !visiting.insert(id) {
            return 0; // cycle guard — shouldn't happen for a well-formed DAG
        }
        let best = preds
            .get(&id)
            .into_iter()
            .flatten()
            .map(|&p| longest_to(p, preds, memo, visiting))
            .max()
            .unwrap_or(0);
        visiting.remove(&id);
        memo.insert(id, best + 1);
        best + 1
    }

    let mut memo = HashMap::new();
    let mut visiting = HashSet::new();
    let depths: HashMap<TaskId, usize> = node_ids
        .iter()
        .map(|&id| (id, longest_to(id, &preds, &mut memo, &mut visiting)))
        .collect();
    let Some(&max_depth) = depths.values().max() else {
        return HashSet::new();
    };

    // Walk back from every node at max depth along a deepest predecessor —
    // there can be multiple longest chains; this highlights one of them
    // (or several, if they happen to share nodes) rather than picking
    // arbitrarily among ties in a way that looks inconsistent.
    let mut on_path: HashSet<TaskId> = HashSet::new();
    for (&id, &depth) in &depths {
        if depth == max_depth {
            let mut cur = id;
            on_path.insert(cur);
            loop {
                let Some(preds_of_cur) = preds.get(&cur) else {
                    break;
                };
                let next = preds_of_cur
                    .iter()
                    .copied()
                    .find(|p| depths.get(p).copied().unwrap_or(0) + 1 == depths[&cur]);
                match next {
                    Some(p) => {
                        on_path.insert(p);
                        cur = p;
                    }
                    None => break,
                }
            }
        }
    }
    on_path
}

#[derive(Clone)]
struct PlanGraphBundle {
    graph: api::PlanGraph,
    waves: Vec<api::PlanFanoutWave>,
    progress: Option<api::PlanProgressSummary>,
}

/// Cancel-on-cleanup: reads component-owned signals after the await, so a
/// plain spawn would panic if the route is disposed mid-fetch. See
/// task_list.rs for the full rationale.
fn spawn_graph_fetch(
    plan_id: String,
    graph_data: RwSignal<Option<Result<PlanGraphBundle, String>>>,
) {
    leptos::task::spawn_local_scoped_with_cancellation(async move {
        let result = fetch_plan_graph_bundle(&plan_id).await;
        graph_data.set(Some(result));
    });
}

async fn fetch_plan_graph_bundle(plan_id: &str) -> Result<PlanGraphBundle, String> {
    let graph = api::plan_graph(plan_id).await.map_err(|e| e.friendly())?;
    let waves = api::plan_fanout(plan_id).await.map_err(|e| e.friendly())?;
    // Best-effort: a summary line is a nice-to-have, not worth failing the
    // whole subpanel over if this one call has a bad day.
    let progress = api::plan_progress(plan_id).await.ok();
    Ok(PlanGraphBundle {
        graph,
        waves,
        progress,
    })
}

fn render_plan_graph(bundle: &PlanGraphBundle) -> AnyView {
    if bundle.graph.nodes.is_empty() {
        return view! {
            <p class="plan-graph-empty">"No tasks in this plan yet."</p>
        }
        .into_any();
    }

    let critical = critical_path(&bundle.graph.nodes, &bundle.graph.edges);
    let node_by_id: HashMap<TaskId, &api::PlanGraphNode> =
        bundle.graph.nodes.iter().map(|n| (n.task_id, n)).collect();
    let deps_count: HashMap<TaskId, usize> = bundle
        .graph
        .nodes
        .iter()
        .map(|n| (n.task_id, n.depends_on.len()))
        .collect();

    let summary = bundle.progress.as_ref().map(|p| {
        let next = p
            .next_ready
            .and_then(|id| node_by_id.get(&id))
            .map(|n| format!(" · next: {}", n.title))
            .unwrap_or_default();
        format!(
            "{}/{} done · {} in progress{}",
            p.done, p.total, p.in_progress, next
        )
    });

    let waves_view: Option<AnyView> = if bundle.waves.is_empty() {
        None
    } else {
        let wave_rows: Vec<AnyView> = bundle
            .waves
            .iter()
            .map(|w| {
                let chips: Vec<AnyView> = w
                    .tasks
                    .iter()
                    .filter_map(|id| node_by_id.get(id))
                    .map(|n| task_chip(n, critical.contains(&n.task_id)))
                    .collect();
                view! {
                    <div class="plan-graph-wave">
                        <span class="plan-graph-wave__label">{format!("Wave {}", w.wave)}</span>
                        <div class="plan-graph-wave__tasks">{chips}</div>
                    </div>
                }
                .into_any()
            })
            .collect();
        Some(
            view! {
                <div class="plan-graph-waves">{wave_rows}</div>
            }
            .into_any(),
        )
    };

    // Full task list (position order) — waves only cover *remaining* work,
    // so this is what still shows the shape of an already-completed plan.
    let mut nodes_sorted: Vec<&api::PlanGraphNode> = bundle.graph.nodes.iter().collect();
    nodes_sorted.sort_by_key(|n| n.position);
    let node_rows: Vec<AnyView> = nodes_sorted
        .into_iter()
        .map(|n| {
            let is_critical = critical.contains(&n.task_id);
            let row_class = if is_critical {
                "plan-graph-node plan-graph-node--critical"
            } else {
                "plan-graph-node"
            };
            let deps = deps_count.get(&n.task_id).copied().unwrap_or(0);
            view! {
                <div class=row_class>
                    <span class="plan-graph-node__pos">{n.position}</span>
                    <span class="plan-graph-node__title">{n.title.clone()}</span>
                    <span class=task_status_class(n.status)>{task_status_label(n.status)}</span>
                    <span class="plan-graph-node__deps">
                        { if deps > 0 { format!("depends on {deps}") } else { String::new() } }
                    </span>
                    <span class="plan-graph-node__id">{format!("#{}", short_id(&n.task_id.to_string()))}</span>
                </div>
            }
            .into_any()
        })
        .collect();

    view! {
        <div class="plan-graph">
            { summary.map(|s| view! { <div class="plan-graph-summary">{s}</div> }) }
            { waves_view }
            <div class="plan-graph-nodes">{node_rows}</div>
        </div>
    }
    .into_any()
}

/// One task chip inside a wave — compact, just title + status + critical marker.
fn task_chip(node: &api::PlanGraphNode, is_critical: bool) -> AnyView {
    let class = if is_critical {
        "plan-graph-task-chip plan-graph-task-chip--critical"
    } else {
        "plan-graph-task-chip"
    };
    view! {
        <span class=class title=task_status_label(node.status).to_string()>
            {node.title.clone()}
        </span>
    }
    .into_any()
}

// ── Treeview renderer ─────────────────────────────────────────────────────────
//
// Plain function (not #[component]) so it can recurse without type-system issues.
// Returns AnyView for uniform type across recursion levels.

/// `graph_refresh` is bumped by `PlansPanel` on any `Channel::Plans`/
/// `Channel::Tasks` event; each row's graph subpanel (if open and already
/// loaded once) silently refetches when it sees a bump. Threaded through
/// the recursion like `depth`.
fn plan_node_view(node: PlanTreeNode, depth: usize, graph_refresh: RwSignal<u32>) -> AnyView {
    let has_children = !node.children.is_empty();
    let plan = node.plan;
    let expanded = RwSignal::new(true);

    // Render children eagerly; toggling is done via CSS display:none/block only.
    let children_views: Vec<AnyView> = node
        .children
        .into_iter()
        .map(|child| plan_node_view(child, depth + 1, graph_refresh))
        .collect();

    let title = plan.title.clone();
    let status = plan.status;
    let criteria_count = plan.success_criteria.len();
    let sc = status_class(&status);
    let sl = status_label(&status);
    let is_abandoned = status == PlanStatus::Abandoned || plan.archived_at.is_some();
    let plan_id = plan.id.to_string();

    // Inline CSS custom property drives depth-based indent in stylesheet:
    //   padding-left: calc(var(--depth, 0) * 1rem + 0.6rem)
    let depth_style = format!("--depth:{depth}");

    // ── Dependency graph subpanel state (lazy, fetched on first expand) ────
    let graph_open = RwSignal::new(false);
    let graph_data: RwSignal<Option<Result<PlanGraphBundle, String>>> = RwSignal::new(None);

    let on_graph_toggle = {
        let plan_id = plan_id.clone();
        move |_: web_sys::MouseEvent| {
            graph_open.update(|v| *v = !*v);
            if graph_open.get_untracked() && graph_data.get_untracked().is_none() {
                spawn_graph_fetch(plan_id.clone(), graph_data);
            }
        }
    };

    // Live refresh: a relevant event landed while this subpanel is open and
    // already has data — refetch quietly. Guarded so this doesn't fire on
    // its own initial creation (both conditions are false until the user
    // has actually opened + loaded the panel at least once).
    Effect::new(move |_| {
        graph_refresh.get();
        if graph_open.get_untracked() && graph_data.get_untracked().is_some() {
            spawn_graph_fetch(plan_id.clone(), graph_data);
        }
    });

    view! {
        <div class=if is_abandoned { "plan-tree-node archived" } else { "plan-tree-node" }>
            <div class="plan-row plan-tree-row" style=depth_style>
                // Chevron: ▶ collapsed / ▼ expanded / non-breaking space for leaf nodes
                <span
                    class="plan-chevron"
                    on:click=move |_| {
                        if has_children {
                            expanded.update(|v| *v = !*v);
                        }
                    }
                >
                    {move || {
                        if !has_children {
                            "\u{a0}"
                        } else if expanded.get() {
                            "▼"
                        } else {
                            "▶"
                        }
                    }}
                </span>
                <span class="plan-title">{title}</span>
                <span class=sc>{sl}</span>
                // Progress indicator per level: success criteria count (fetching
                // full PlanProgress per plan is expensive; criteria count is in-band).
                <span class="plan-pct" title="success criteria count">
                    {format!("{criteria_count} sc")}
                </span>
                <button
                    class="plan-graph-toggle"
                    type="button"
                    on:click=on_graph_toggle
                >
                    {move || if graph_open.get() { "graph ▴" } else { "graph ▾" }}
                </button>
            </div>
            <Show when=move || graph_open.get() fallback=|| view! { <></> }>
                <div class="plan-graph-panel">
                    {move || match graph_data.get() {
                        None => view! {
                            <div class="plan-graph-loading">"loading graph…"</div>
                        }.into_any(),
                        Some(Err(err)) => view! {
                            <p class="fetch-error__message">{err}</p>
                        }.into_any(),
                        Some(Ok(bundle)) => render_plan_graph(&bundle),
                    }}
                </div>
            </Show>
            // Children container: rendered once, shown/hidden via display property only.
            <div
                class="plan-children"
                style=move || {
                    if !has_children || !expanded.get() {
                        "display:none"
                    } else {
                        ""
                    }
                }
            >
                {children_views}
            </div>
        </div>
    }
    .into_any()
}

// ── PlansPanel ────────────────────────────────────────────────────────────────

#[component]
pub fn PlansPanel() -> impl IntoView {
    let projects_ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let ws_ctx = use_context::<WsCtx>().expect("WsCtx");
    let current_filter = projects_ctx.current_filter;
    let ws_events = ws_ctx.events;

    // Derive project_id from filter — only Some when Of(pid).
    let project_id_opt = Memo::new(move |_| match current_filter.get() {
        ProjectFilter::Of(pid) => Some(pid.to_string()),
        _ => None,
    });

    // Per-project plan cache, kept in sync via WS apply.
    let cache: RwSignal<HashMap<String, Vec<Plan>>> = RwSignal::new(HashMap::new());
    let applied_cursor: RwSignal<usize> = RwSignal::new(0);
    let fetch_seq: RwSignal<HashMap<String, u64>> = RwSignal::new(HashMap::new());
    // Most recent fetch failure for the current project, if any — the fetch
    // below still caches an empty Vec on failure (unchanged behavior), this
    // just remembers *why* so the view can show it instead of "No plans yet."
    let fetch_error: RwSignal<Option<String>> = RwSignal::new(None);

    let plans: Memo<Vec<Plan>> = Memo::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return Vec::new();
        };
        cache.with(|m| m.get(&pid).cloned().unwrap_or_default())
    });

    let loaded: Memo<bool> = Memo::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return true;
        };
        cache.with(|m| m.contains_key(&pid))
    });

    // 1) Fetch only on first visit to a project — cache hit reuses WS-applied
    //    snapshot.
    Effect::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return;
        };
        // Clear before the cache-hit check below: `fetch_error` isn't keyed
        // per-project, so a stale error from a previous failed project must
        // not linger over a different project that's actually a cache hit.
        fetch_error.set(None);
        if cache.with_untracked(|m| m.contains_key(&pid)) {
            return;
        }
        let snapshot_at = ws_events.with_untracked(|v| v.len());
        let my_seq = fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0)) + 1;
        fetch_seq.update(|m| {
            m.insert(pid.clone(), my_seq);
        });

        // Cancel-on-cleanup: the future reads component-owned signals
        // (`fetch_seq`) after the await, so a plain spawn would panic if the
        // route is disposed mid-fetch. See task_list.rs for the full rationale.
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let mut ps = match api::list_plans(&pid).await {
                Ok(ps) => ps,
                Err(err) => {
                    leptos::logging::log!("list_plans failed for project={pid}: {err:?}");
                    fetch_error.set(Some(err.friendly()));
                    Vec::new()
                }
            };
            // Catch up to events that arrived during the in-flight fetch.
            ws_events.with_untracked(|evs| {
                let now_len = evs.len();
                if snapshot_at < now_len {
                    for env in &evs[snapshot_at..now_len] {
                        apply_plan_event(env, &mut ps, &pid);
                    }
                }
            });

            let latest_seq = fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0));
            if latest_seq != my_seq {
                return;
            }

            cache.update(|m| {
                m.insert(pid.clone(), ps);
            });
        });
    });

    // 2) Apply WS events to every cached snapshot.
    Effect::new(move |_| {
        let len = ws_events.with(|v| v.len());
        let start = applied_cursor.get_untracked();
        if start >= len {
            return;
        }
        ws_events.with_untracked(|evs| {
            cache.update(|m| {
                for env in &evs[start..len] {
                    for (pid, list) in m.iter_mut() {
                        apply_plan_event(env, list, pid);
                    }
                }
            });
        });
        applied_cursor.set(len);
    });

    // 3) Bump `graph_refresh` on any Channel::Plans/Tasks event, so every
    // open dependency-graph subpanel (see `plan_node_view`) knows to
    // refetch. Plans events cover graph-shape changes (tasks added/removed/
    // reordered); Tasks events cover the status changes that drive "where
    // is execution now" — the graph endpoint bundles task status directly,
    // there's no separate live task cache to patch it from in place.
    let graph_refresh: RwSignal<u32> = RwSignal::new(0);
    let graph_applied_cursor: RwSignal<usize> = RwSignal::new(0);
    Effect::new(move |_| {
        let len = ws_events.with(|v| v.len());
        let start = graph_applied_cursor.get_untracked();
        if start >= len {
            return;
        }
        let relevant = ws_events.with_untracked(|evs| {
            evs[start..len].iter().any(|env: &EventEnvelope| {
                matches!(env.payload.channel(), Channel::Plans | Channel::Tasks)
            })
        });
        graph_applied_cursor.set(len);
        if relevant {
            graph_refresh.update(|n| *n = n.wrapping_add(1));
        }
    });

    view! {
        {move || {
            match current_filter.get() {
                ProjectFilter::Of(_) => {
                    view! {
                        <div class="plans-panel">
                            <div class="plans-header">
                                <span class="plans-title">"Plans"</span>
                            </div>
                            <Show
                                when=move || loaded.get()
                                fallback=|| view! { <div class="plans-empty">"Loading…"</div> }
                            >
                                {move || {
                                    let ps = plans.get();
                                    if let Some(err) = fetch_error.get() {
                                        view! {
                                            <p class="fetch-error__message">{err}</p>
                                        }.into_any()
                                    } else if ps.is_empty() {
                                        view! {
                                            <p class="plans-empty">"No plans yet."</p>
                                        }.into_any()
                                    } else {
                                        let groups: Vec<AnyView> = group_roots_by_status(build_tree(ps))
                                            .into_iter()
                                            .map(|(status, group_nodes)| {
                                                let count = group_nodes.len();
                                                let header_class = format!(
                                                    "plan-group__header plan-group__header--{}",
                                                    plan_group_slug(status),
                                                );
                                                let nodes: Vec<AnyView> = group_nodes
                                                    .into_iter()
                                                    .map(|node| plan_node_view(node, 0, graph_refresh))
                                                    .collect();
                                                view! {
                                                    <section class="plan-group">
                                                        <div class=header_class>
                                                            <span class="plan-group__label">
                                                                {plan_group_label(status)}
                                                            </span>
                                                            <span class="plan-group__count">{count}</span>
                                                        </div>
                                                        {nodes}
                                                    </section>
                                                }
                                                .into_any()
                                            })
                                            .collect();
                                        view! {
                                            <div class="plan-tree">{groups}</div>
                                        }.into_any()
                                    }
                                }}
                            </Show>
                        </div>
                    }
                    .into_any()
                }
                _ => view! { <div class="plans-aside-hidden" /> }.into_any(),
            }
        }}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daruma_domain::Actor;
    use daruma_shared::{time, PlanId, ProjectId};

    fn node(title: &str, status: PlanStatus) -> PlanTreeNode {
        let now = time::now();
        PlanTreeNode {
            plan: Plan {
                id: PlanId::new(),
                project_id: ProjectId::new(),
                parent_plan_id: None,
                title: title.to_string(),
                description: String::new(),
                goal: String::new(),
                success_criteria: Vec::new(),
                status,
                owner: Actor::user(),
                created_at: now,
                updated_at: now,
                archived_at: None,
                source_brief: None,
            },
            children: Vec::new(),
        }
    }

    #[test]
    fn group_roots_by_status_orders_groups_and_preserves_root_order() {
        let groups = group_roots_by_status(vec![
            node("draft", PlanStatus::Draft),
            node("active-1", PlanStatus::Active),
            node("completed", PlanStatus::Completed),
            node("active-2", PlanStatus::Active),
        ]);

        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].0, PlanStatus::Active);
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[0].1[0].plan.title, "active-1");
        assert_eq!(groups[0].1[1].plan.title, "active-2");
        assert_eq!(groups[1].0, PlanStatus::Draft);
        assert_eq!(groups[1].1.len(), 1);
        assert_eq!(groups[2].0, PlanStatus::Completed);
        assert_eq!(groups[2].1.len(), 1);
    }
}
