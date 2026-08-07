use super::fmt;
use crate::api;
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::ws::WsCtx;
use daruma_domain::{Actor, Plan, PlanPatch, PlanStatus};
use daruma_events::{Channel, Event, EventEnvelope};
use daruma_shared::time::Timestamp;
use daruma_shared::TaskId;
use leptos::prelude::*;
use std::collections::{HashMap, HashSet};

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

#[derive(Clone)]
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

/// Task-status pill class/label come from `super::fmt` — `PlanGraphNode.status`
/// is the same `Status` enum task_list.rs/task_row.rs use, so the existing
/// `.status-*` colors apply with no new CSS.

// ── Flow layout ──────────────────────────────────────────────────────────────
//
// Geometry of the plan diagram. A column is one dependency rank, a row is one
// task. Everything is derived from the data — same plan, same picture, every
// time — because a layout that moves between loads is unreadable no matter how
// few nodes it has.

const FLOW_COL_PITCH: f64 = 200.0;
const FLOW_BOX_W: f64 = 168.0;
const FLOW_BOX_H: f64 = 44.0;
const FLOW_ROW_PITCH: f64 = 60.0;
const FLOW_PAD: f64 = 14.0;
/// Characters that fit in a box at 11px monospace.
const FLOW_TITLE_CHARS: usize = 22;

/// One placed task box.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowBox {
    pub task_id: TaskId,
    pub rank: usize,
    pub row: usize,
    pub x: f64,
    pub y: f64,
}

/// Deterministic left-to-right layout of a plan's task DAG.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanFlow {
    pub boxes: Vec<FlowBox>,
    pub width: f64,
    pub height: f64,
}

impl PlanFlow {
    fn place(&self, id: TaskId) -> Option<&FlowBox> {
        self.boxes.iter().find(|b| b.task_id == id)
    }
}

/// Rank every task by its longest dependency chain, then order within each rank
/// so that a task sits near the tasks it depends on.
///
/// Ranking by longest path (not by the server's fanout waves) on purpose: waves
/// only cover *remaining* work, so a half-done plan would lose the shape of
/// everything already finished. Ordering uses the median predecessor row — one
/// pass of the standard crossing-reduction heuristic, which is the difference
/// between a diagram and a bundle of diagonals once a plan has any fan-in.
pub fn plan_flow_layout(nodes: &[api::PlanGraphNode], edges: &[api::PlanGraphEdge]) -> PlanFlow {
    if nodes.is_empty() {
        return PlanFlow::default();
    }
    let ranks = dependency_ranks(nodes, edges);
    let preds = predecessors(nodes, edges);

    let max_rank = ranks.values().copied().max().unwrap_or(0);
    let mut rows_of: HashMap<TaskId, usize> = HashMap::new();
    let mut boxes: Vec<FlowBox> = Vec::with_capacity(nodes.len());
    let mut widest_rank = 0usize;

    for rank in 0..=max_rank {
        let mut in_rank: Vec<&api::PlanGraphNode> = nodes
            .iter()
            .filter(|n| ranks.get(&n.task_id).copied().unwrap_or(0) == rank)
            .collect();

        // Median of already-placed predecessor rows; `position` breaks ties and
        // orders the first column, so the result is fully determined.
        in_rank.sort_by(|a, b| {
            let key = |n: &api::PlanGraphNode| {
                let mut prows: Vec<usize> = preds
                    .get(&n.task_id)
                    .into_iter()
                    .flatten()
                    .filter_map(|p| rows_of.get(p).copied())
                    .collect();
                prows.sort_unstable();
                prows.get(prows.len() / 2).copied()
            };
            key(a)
                .cmp(&key(b))
                .then_with(|| a.position.cmp(&b.position))
                .then_with(|| a.task_id.cmp(&b.task_id))
        });

        widest_rank = widest_rank.max(in_rank.len());
        for (row, n) in in_rank.iter().enumerate() {
            rows_of.insert(n.task_id, row);
            boxes.push(FlowBox {
                task_id: n.task_id,
                rank,
                row,
                x: FLOW_PAD + rank as f64 * FLOW_COL_PITCH,
                y: FLOW_PAD + row as f64 * FLOW_ROW_PITCH,
            });
        }
    }

    PlanFlow {
        width: FLOW_PAD * 2.0 + max_rank as f64 * FLOW_COL_PITCH + FLOW_BOX_W,
        height: FLOW_PAD * 2.0 + widest_rank.saturating_sub(1) as f64 * FLOW_ROW_PITCH + FLOW_BOX_H,
        boxes,
    }
}

/// blocker -> blocked adjacency, restricted to tasks present in the graph.
fn predecessors(
    nodes: &[api::PlanGraphNode],
    edges: &[api::PlanGraphEdge],
) -> HashMap<TaskId, Vec<TaskId>> {
    let node_ids: HashSet<TaskId> = nodes.iter().map(|n| n.task_id).collect();
    let mut preds: HashMap<TaskId, Vec<TaskId>> = HashMap::new();
    for e in edges {
        if node_ids.contains(&e.from) && node_ids.contains(&e.to) {
            preds.entry(e.to).or_default().push(e.from);
        }
    }
    // `depends_on` on the node itself is the same relation from the other side;
    // the server fills both, and a task missing from `edges` must still rank
    // behind its blockers.
    for n in nodes {
        let entry = preds.entry(n.task_id).or_default();
        for dep in &n.depends_on {
            if node_ids.contains(dep) && !entry.contains(dep) {
                entry.push(*dep);
            }
        }
    }
    for v in preds.values_mut() {
        v.sort_unstable();
    }
    preds
}

/// 0-based longest-path depth per task. Cycle-safe (see `critical_path`).
fn dependency_ranks(
    nodes: &[api::PlanGraphNode],
    edges: &[api::PlanGraphEdge],
) -> HashMap<TaskId, usize> {
    let preds = predecessors(nodes, edges);
    let mut memo = HashMap::new();
    let mut visiting = HashSet::new();
    nodes
        .iter()
        .map(|n| {
            (
                n.task_id,
                longest_to(n.task_id, &preds, &mut memo, &mut visiting) - 1,
            )
        })
        .collect()
}

/// Longest chain ending at `id`, counting `id` itself (so a root is 1).
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

/// Longest dependency chain through the graph (unweighted — number of hops),
/// as a set for O(1) "is this task on the critical path" lookups. `edges`
/// point blocker -> blocked (both `depends_on` and `blocks` share that
/// direction, per `plan_readiness::plan_graph`'s construction server-side),
/// so this is a standard DAG longest-path via memoized DFS. A `visiting`
/// guard makes it safe against a malformed cyclic input instead of
/// stack-overflowing the tab.
fn critical_path(nodes: &[api::PlanGraphNode], edges: &[api::PlanGraphEdge]) -> HashSet<TaskId> {
    let node_ids: HashSet<TaskId> = nodes.iter().map(|n| n.task_id).collect();
    let preds = predecessors(nodes, edges);

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
pub(crate) struct PlanGraphBundle {
    graph: api::PlanGraph,
    waves: Vec<api::PlanFanoutWave>,
    progress: Option<api::PlanProgressSummary>,
}

/// Cancel-on-cleanup: reads component-owned signals after the await, so a
/// plain spawn would panic if the route is disposed mid-fetch. See
/// task_list.rs for the full rationale.
pub(crate) fn spawn_graph_fetch(
    plan_id: String,
    graph_data: RwSignal<Option<Result<PlanGraphBundle, String>>>,
) {
    leptos::task::spawn_local_scoped_with_cancellation(async move {
        let result = fetch_plan_graph_bundle(&plan_id).await;
        graph_data.set(Some(result));
    });
}

pub(crate) async fn fetch_plan_graph_bundle(plan_id: &str) -> Result<PlanGraphBundle, String> {
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

pub(crate) fn render_plan_graph(bundle: &PlanGraphBundle) -> AnyView {
    if bundle.graph.nodes.is_empty() {
        return view! {
            <p class="plan-graph-empty">"No tasks in this plan yet."</p>
        }
        .into_any();
    }

    let critical = critical_path(&bundle.graph.nodes, &bundle.graph.edges);
    let node_by_id: HashMap<TaskId, &api::PlanGraphNode> =
        bundle.graph.nodes.iter().map(|n| (n.task_id, n)).collect();
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

    // Ready-now set: fanout wave 0 is what can be claimed immediately. Marked
    // on the diagram rather than listed separately.
    let ready_now: HashSet<TaskId> = bundle
        .waves
        .iter()
        .filter(|w| w.wave == 0)
        .flat_map(|w| w.tasks.iter().copied())
        .collect();

    // The wave list used to sit under this: same information the columns now
    // carry, in a second encoding. The only part it said that the columns don't
    // is "claimable right now", and that is the accent outline on wave-0 boxes.

    view! {
        <div class="plan-graph">
            { summary.map(|s| view! { <div class="plan-graph-summary">{s}</div> }) }
            <PlanFlowCanvas
                graph=bundle.graph.clone()
                critical=critical
                ready_now=ready_now
            />
        </div>
    }
    .into_any()
}

/// Client coordinates expressed relative to the canvas element the handler is
/// bound to. Falls back to the raw point if the target is not an element.
fn canvas_offset(target: Option<web_sys::EventTarget>, client_x: i32, client_y: i32) -> (f64, f64) {
    use wasm_bindgen::JsCast;
    match target.and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
        Some(el) => {
            let rect = el.get_bounding_client_rect();
            (client_x as f64 - rect.left(), client_y as f64 - rect.top())
        }
        None => (client_x as f64, client_y as f64),
    }
}

/// Pan/zoom viewport around the flow diagram.
///
/// A fixed box with scrollbars caps how much of a plan you can take in at once
/// and forces two-axis scrolling to follow one chain. This is a window onto an
/// unbounded field instead: drag to move, wheel to zoom about the cursor. The
/// diagram's own coordinates never change — only the transform on top of them —
/// so the layout stays the deterministic thing it was.
#[component]
fn PlanFlowCanvas(
    graph: api::PlanGraph,
    critical: HashSet<TaskId>,
    ready_now: HashSet<TaskId>,
) -> impl IntoView {
    let flow = plan_flow_layout(&graph.nodes, &graph.edges);
    let content = render_plan_flow(&flow, &graph, &critical, &ready_now);

    // (scale, translate_x, translate_y)
    let view_box: RwSignal<(f64, f64, f64)> = RwSignal::new((1.0, 0.0, 0.0));
    // Pointer position at the last drag sample, in client coordinates.
    let drag_from: RwSignal<Option<(f64, f64)>> = RwSignal::new(None);

    let on_pointer_down = move |ev: web_sys::PointerEvent| {
        if ev.button() != 0 {
            return;
        }
        drag_from.set(Some((ev.client_x() as f64, ev.client_y() as f64)));
    };
    let on_pointer_move = move |ev: web_sys::PointerEvent| {
        let Some((lx, ly)) = drag_from.get_untracked() else {
            return;
        };
        let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
        view_box.update(|(_, tx, ty)| {
            *tx += x - lx;
            *ty += y - ly;
        });
        drag_from.set(Some((x, y)));
    };
    let on_pointer_up = move |_: web_sys::PointerEvent| drag_from.set(None);

    let on_wheel = move |ev: web_sys::WheelEvent| {
        ev.prevent_default();
        // Zoom about the cursor: the point under it must not move, so the
        // translation absorbs the scale change around that anchor.
        //
        // The anchor is measured against the canvas, not `offset_x` — that is
        // relative to whatever child the pointer happens to be over, so zooming
        // with the cursor on a task box would anchor inside that box and make
        // the diagram jump.
        let (ax, ay) = canvas_offset(ev.current_target(), ev.client_x(), ev.client_y());
        view_box.update(|(k, tx, ty)| {
            let factor = if ev.delta_y() < 0.0 { 1.1 } else { 1.0 / 1.1 };
            let next = (*k * factor).clamp(0.2, 3.0);
            let applied = next / *k;
            *tx = ax - (ax - *tx) * applied;
            *ty = ay - (ay - *ty) * applied;
            *k = next;
        });
    };

    view! {
        <div class="plan-flow">
            <button
                class="plan-flow__reset btn-ghost btn-sm"
                type="button"
                title="Reset pan and zoom"
                on:click=move |_| view_box.set((1.0, 0.0, 0.0))
            >
                "⊞ Reset"
            </button>
            <svg
                class="plan-flow__svg"
                class:plan-flow__svg--dragging=move || drag_from.get().is_some()
                on:pointerdown=on_pointer_down
                on:pointermove=on_pointer_move
                on:pointerup=on_pointer_up
                on:pointerleave=on_pointer_up
                on:wheel=on_wheel
            >
                <defs>
                    <marker
                        id="plan-flow-arrow"
                        markerWidth="7" markerHeight="7"
                        refX="6" refY="3.5" orient="auto"
                    >
                        <path d="M0,0 L7,3.5 L0,7 z" class="plan-flow__arrowhead" />
                    </marker>
                </defs>
                <g transform=move || {
                    let (k, tx, ty) = view_box.get();
                    format!("translate({tx},{ty}) scale({k})")
                }>
                    {content}
                </g>
            </svg>
        </div>
    }
}

/// The plan as a left-to-right flow: one column per dependency rank, one box
/// per task, orthogonal arrows for `depends_on`.
///
/// Hand-rolled SVG, no graph-visualization dependency: the layout is a sort and
/// two arithmetic expressions, and every coordinate comes from the data, so the
/// picture is stable across loads.
fn render_plan_flow(
    flow: &PlanFlow,
    graph: &api::PlanGraph,
    critical: &HashSet<TaskId>,
    ready_now: &HashSet<TaskId>,
) -> AnyView {
    if flow.boxes.is_empty() {
        return view! { <></> }.into_any();
    }
    let node_by_id: HashMap<TaskId, &api::PlanGraphNode> =
        graph.nodes.iter().map(|n| (n.task_id, n)).collect();

    // Edges first so boxes paint over the arrowheads' tails.
    let mut seen: HashSet<(TaskId, TaskId)> = HashSet::new();
    let edge_views: Vec<AnyView> = graph
        .edges
        .iter()
        .filter(|e| seen.insert((e.from, e.to)))
        .filter_map(|e| {
            let from = flow.place(e.from)?;
            let to = flow.place(e.to)?;
            let sx = from.x + FLOW_BOX_W;
            let sy = from.y + FLOW_BOX_H / 2.0;
            let ex = to.x;
            let ey = to.y + FLOW_BOX_H / 2.0;
            // Elbow halfway between the columns — right angles read as a
            // diagram; straight diagonals read as the web we just left.
            let mx = (sx + ex) / 2.0;
            let d = format!("M {sx} {sy} H {mx} V {ey} H {ex}");
            let on_critical = critical.contains(&e.from) && critical.contains(&e.to);
            let class = if on_critical {
                "plan-flow__edge plan-flow__edge--critical"
            } else {
                "plan-flow__edge"
            };
            Some(view! { <path class=class d=d marker-end="url(#plan-flow-arrow)" /> }.into_any())
        })
        .collect();

    let box_views: Vec<AnyView> = flow
        .boxes
        .iter()
        .filter_map(|b| {
            let n = node_by_id.get(&b.task_id)?;
            let mut class = String::from("plan-flow__node");
            if critical.contains(&b.task_id) {
                class.push_str(" plan-flow__node--critical");
            }
            if ready_now.contains(&b.task_id) {
                class.push_str(" plan-flow__node--ready");
            }
            let short = fmt::short_id(&b.task_id.to_string());
            let tooltip = format!("{} · {} · #{short}", n.title, fmt::status_label(n.status));
            let title = truncate_chars(&n.title, FLOW_TITLE_CHARS);
            let status_class = format!("plan-flow__status status-{}", status_slug(n.status));
            Some(
                view! {
                    <g class=class transform=format!("translate({},{})", b.x, b.y)>
                        <title>{tooltip}</title>
                        <rect class="plan-flow__box" width=FLOW_BOX_W height=FLOW_BOX_H rx="5" />
                        <text class="plan-flow__title" x="9" y="18">{title}</text>
                        <text class=status_class x="9" y="33">{fmt::status_label(n.status)}</text>
                        <text class="plan-flow__id" x=FLOW_BOX_W - 9.0 y="33" text-anchor="end">
                            {format!("#{short}")}
                        </text>
                    </g>
                }
                .into_any(),
            )
        })
        .collect();

    // Content only — `PlanFlowCanvas` owns the <svg>, its <defs> and the
    // pan/zoom transform this sits inside.
    view! {
        <>
            {edge_views}
            {box_views}
        </>
    }
    .into_any()
}

/// CSS-friendly discriminant for a task status.
fn status_slug(status: daruma_domain::Status) -> &'static str {
    use daruma_domain::Status;
    match status {
        Status::Inbox => "inbox",
        Status::Todo => "todo",
        Status::InProgress => "in-progress",
        Status::InReview => "in-review",
        Status::Done => "done",
        Status::Cancelled => "cancelled",
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        chars[..max.saturating_sub(1)].iter().collect::<String>() + "…"
    }
}

// ── Run timeline (VIZ-6, run half) ──────────────────────────────────────────
//
// Separate lazy subpanel from the dependency graph above (its own toggle),
// since a plan can carry several runs and cramming both into one subpanel
// gets noisy fast. Live refresh shares the same "bump a counter, refetch
// what's open" approach as the graph subpanel, keyed to Channel::Runs
// instead of Plans/Tasks — that one channel covers run status, step
// progress, and note appends alike (see `PlansPanel`'s watcher effect).

fn run_status_class(status: api::RunStatus) -> &'static str {
    match status {
        api::RunStatus::Active => "run-status run-status-active",
        api::RunStatus::Completed => "run-status run-status-completed",
        api::RunStatus::Failed => "run-status run-status-failed",
        api::RunStatus::Aborted => "run-status run-status-aborted",
    }
}

fn run_status_label(status: api::RunStatus) -> &'static str {
    match status {
        api::RunStatus::Active => "active",
        api::RunStatus::Completed => "completed",
        api::RunStatus::Failed => "failed",
        api::RunStatus::Aborted => "aborted",
    }
}

/// "user" or the agent's display name — same convention as
/// `activity_feed.rs`'s private `actor_label`, copied rather than shared.
fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::User => "user".to_string(),
        Actor::Agent { name, .. } => name.clone(),
    }
}

/// Task title if the plan's dependency graph happens to be loaded already
/// (cheap — no extra fetch), else a short id. Reads `graph_data` with the
/// tracked `.get()`, so a step's title upgrades live if the user opens the
/// graph subpanel *after* the run timeline.
fn resolve_task_title(
    task_id: TaskId,
    graph_data: RwSignal<Option<Result<PlanGraphBundle, String>>>,
) -> String {
    graph_data
        .get()
        .and_then(|r| r.ok())
        .and_then(|bundle| {
            bundle
                .graph
                .nodes
                .iter()
                .find(|n| n.task_id == task_id)
                .map(|n| n.title.clone())
        })
        .unwrap_or_else(|| format!("#{}", fmt::short_id(&task_id.to_string())))
}

fn outcome_badge(outcome: &Option<api::RunOutcome>) -> AnyView {
    match outcome {
        None => view! {
            <span class="plan-run-step__outcome plan-run-step__outcome--pending">"running…"</span>
        }
        .into_any(),
        Some(api::RunOutcome::Done) => view! {
            <span class="plan-run-step__outcome plan-run-step__outcome--done">"done"</span>
        }
        .into_any(),
        Some(api::RunOutcome::HumanCompleted) => view! {
            <span class="plan-run-step__outcome plan-run-step__outcome--done">"human completed"</span>
        }
        .into_any(),
        Some(api::RunOutcome::Superseded) => view! {
            <span class="plan-run-step__outcome plan-run-step__outcome--other">"superseded"</span>
        }
        .into_any(),
        Some(api::RunOutcome::Skipped) => view! {
            <span class="plan-run-step__outcome plan-run-step__outcome--other">"skipped"</span>
        }
        .into_any(),
        Some(api::RunOutcome::Failed { reason }) => view! {
            <span class="plan-run-step__outcome plan-run-step__outcome--failed" title=reason.clone()>
                "failed"
            </span>
        }
        .into_any(),
    }
}

fn spawn_timeline_fetch(
    run_id: String,
    timeline: RwSignal<Option<Result<api::RunTimeline, String>>>,
) {
    leptos::task::spawn_local_scoped_with_cancellation(async move {
        let result = api::run_timeline(&run_id).await.map_err(|e| e.friendly());
        timeline.set(Some(result));
    });
}

fn spawn_runs_fetch(plan_id: String, runs_data: RwSignal<Option<Result<Vec<api::Run>, String>>>) {
    leptos::task::spawn_local_scoped_with_cancellation(async move {
        let result = api::list_plan_runs(&plan_id)
            .await
            .map_err(|e| e.friendly());
        runs_data.set(Some(result));
    });
}

fn render_run_timeline(
    tl: &api::RunTimeline,
    graph_data: RwSignal<Option<Result<PlanGraphBundle, String>>>,
) -> AnyView {
    if tl.steps.is_empty() && tl.notes.is_empty() {
        return view! {
            <p class="plan-run-timeline-empty">"No steps recorded yet."</p>
        }
        .into_any();
    }

    let steps: Vec<AnyView> = tl
        .steps
        .iter()
        .map(|step| {
            let task_id = step.task_id;
            let started = fmt::format_ts(step.started_at);
            let finished = step.finished_at.map(fmt::format_ts);
            let outcome = step.outcome.clone();
            let time_range = match finished {
                Some(f) => format!("{started} → {f}"),
                None => format!("{started} → …"),
            };
            view! {
                <div class="plan-run-step">
                    <span class="plan-run-step__task">
                        {move || resolve_task_title(task_id, graph_data)}
                    </span>
                    <span class="plan-run-step__time">{time_range}</span>
                    {outcome_badge(&outcome)}
                </div>
            }
            .into_any()
        })
        .collect();

    let notes: Vec<AnyView> = tl
        .notes
        .iter()
        .map(|note| {
            let meta = format!(
                "{} · {}",
                actor_label(&note.author),
                fmt::format_ts(note.created_at)
            );
            let body = note.body.clone();
            view! {
                <div class="plan-run-note">
                    <span class="plan-run-note__meta">{meta}</span>
                    <p class="plan-run-note__body">{body}</p>
                </div>
            }
            .into_any()
        })
        .collect();

    view! {
        <div class="plan-run-timeline-body">
            <div class="plan-run-steps">{steps}</div>
            { if notes.is_empty() {
                view! { <></> }.into_any()
            } else {
                view! { <div class="plan-run-notes">{notes}</div> }.into_any()
            }}
        </div>
    }
    .into_any()
}

/// One run row: agent, status, started/last-activity — clicking it lazily
/// fetches and expands the run's timeline (steps + notes).
#[component]
fn RunRowView(
    run: api::Run,
    graph_data: RwSignal<Option<Result<PlanGraphBundle, String>>>,
    runs_refresh: RwSignal<u32>,
) -> impl IntoView {
    let expanded = RwSignal::new(false);
    let timeline: RwSignal<Option<Result<api::RunTimeline, String>>> = RwSignal::new(None);
    let run_id = run.id.to_string();

    let on_toggle = {
        let run_id = run_id.clone();
        move |_: web_sys::MouseEvent| {
            expanded.update(|v| *v = !*v);
            if expanded.get_untracked() && timeline.get_untracked().is_none() {
                spawn_timeline_fetch(run_id.clone(), timeline);
            }
        }
    };

    // Live refresh, same guard shape as the graph subpanel's effect.
    Effect::new(move |_| {
        runs_refresh.get();
        if expanded.get_untracked() && timeline.get_untracked().is_some() {
            spawn_timeline_fetch(run_id.clone(), timeline);
        }
    });

    let agent = fmt::short_id(&run.agent_id.to_string());
    let status = run.status;
    let started = fmt::format_ts(run.started_at);
    let last_activity = run.last_activity_at.map(fmt::format_ts);

    view! {
        <li class="plan-run-row-wrapper">
            <div class="plan-run-row" on:click=on_toggle>
                <span class=run_status_class(status)>{run_status_label(status)}</span>
                <span class="plan-run-row__agent">{format!("agent #{agent}")}</span>
                <span class="plan-run-row__started">{format!("started {started}")}</span>
                { last_activity.map(|la| view! {
                    <span class="plan-run-row__activity">{format!("last activity {la}")}</span>
                })}
            </div>
            <Show when=move || expanded.get() fallback=|| view! { <></> }>
                <div class="plan-run-timeline">
                    {move || match timeline.get() {
                        None => view! {
                            <div class="plan-graph-loading">"loading timeline…"</div>
                        }.into_any(),
                        Some(Err(err)) => view! {
                            <p class="fetch-error__message">{err}</p>
                        }.into_any(),
                        Some(Ok(tl)) => render_run_timeline(&tl, graph_data),
                    }}
                </div>
            </Show>
        </li>
    }
}

fn render_plan_runs(
    runs: Vec<api::Run>,
    graph_data: RwSignal<Option<Result<PlanGraphBundle, String>>>,
    runs_refresh: RwSignal<u32>,
) -> AnyView {
    if runs.is_empty() {
        return view! {
            <p class="plan-runs-empty">"No runs yet."</p>
        }
        .into_any();
    }
    let rows: Vec<AnyView> = runs
        .into_iter()
        .map(|run| {
            view! {
                <RunRowView run=run graph_data=graph_data runs_refresh=runs_refresh />
            }
            .into_any()
        })
        .collect();
    view! {
        <ul class="plan-runs-list">{rows}</ul>
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
fn plan_node_view(
    node: PlanTreeNode,
    depth: usize,
    graph_refresh: RwSignal<u32>,
    runs_refresh: RwSignal<u32>,
) -> AnyView {
    let has_children = !node.children.is_empty();
    let plan = node.plan;
    let expanded = RwSignal::new(true);

    // Render children eagerly; toggling is done via CSS display:none/block only.
    let children_views: Vec<AnyView> = node
        .children
        .into_iter()
        .map(|child| plan_node_view(child, depth + 1, graph_refresh, runs_refresh))
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
    {
        let plan_id = plan_id.clone();
        Effect::new(move |_| {
            graph_refresh.get();
            if graph_open.get_untracked() && graph_data.get_untracked().is_some() {
                spawn_graph_fetch(plan_id.clone(), graph_data);
            }
        });
    }

    // ── Runs subpanel state (lazy, fetched on first expand) ────────────────
    let runs_open = RwSignal::new(false);
    let runs_data: RwSignal<Option<Result<Vec<api::Run>, String>>> = RwSignal::new(None);

    let on_runs_toggle = {
        let plan_id = plan_id.clone();
        move |_: web_sys::MouseEvent| {
            runs_open.update(|v| *v = !*v);
            if runs_open.get_untracked() && runs_data.get_untracked().is_none() {
                spawn_runs_fetch(plan_id.clone(), runs_data);
            }
        }
    };

    Effect::new(move |_| {
        runs_refresh.get();
        if runs_open.get_untracked() && runs_data.get_untracked().is_some() {
            spawn_runs_fetch(plan_id.clone(), runs_data);
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
                <div class="plan-row-toggles">
                    <button
                        class="plan-graph-toggle"
                        type="button"
                        on:click=on_graph_toggle
                    >
                        {move || if graph_open.get() { "graph ▴" } else { "graph ▾" }}
                    </button>
                    <button
                        class="plan-graph-toggle"
                        type="button"
                        on:click=on_runs_toggle
                    >
                        {move || if runs_open.get() { "runs ▴" } else { "runs ▾" }}
                    </button>
                </div>
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
            <Show when=move || runs_open.get() fallback=|| view! { <></> }>
                <div class="plan-graph-panel">
                    {move || match runs_data.get() {
                        None => view! {
                            <div class="plan-graph-loading">"loading runs…"</div>
                        }.into_any(),
                        Some(Err(err)) => view! {
                            <p class="fetch-error__message">{err}</p>
                        }.into_any(),
                        Some(Ok(runs)) => render_plan_runs(runs, graph_data, runs_refresh),
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
    let collapsed: RwSignal<HashSet<&'static str>> =
        RwSignal::new(HashSet::from(["completed", "abandoned"]));

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

    // 4) Bump `runs_refresh` on any Channel::Runs event — covers run status,
    // step progress, and note appends alike (see run.rs module docs in the
    // vendored events crate), so a single watch suffices for both the runs
    // list and every open run's timeline (see `plan_node_view`/`RunRowView`).
    let runs_refresh: RwSignal<u32> = RwSignal::new(0);
    let runs_applied_cursor: RwSignal<usize> = RwSignal::new(0);
    Effect::new(move |_| {
        let len = ws_events.with(|v| v.len());
        let start = runs_applied_cursor.get_untracked();
        if start >= len {
            return;
        }
        let relevant = ws_events.with_untracked(|evs| {
            evs[start..len]
                .iter()
                .any(|env: &EventEnvelope| env.payload.channel() == Channel::Runs)
        });
        runs_applied_cursor.set(len);
        if relevant {
            runs_refresh.update(|n| *n = n.wrapping_add(1));
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
                                                let slug = plan_group_slug(status);
                                                let is_collapsed = move || collapsed.get().contains(slug);
                                                let toggle = move |_| {
                                                    collapsed.update(|set| {
                                                        if !set.insert(slug) { set.remove(slug); }
                                                    });
                                                };
                                                let header_class = move || format!(
                                                    "plan-group__header plan-group__header--{}{}",
                                                    slug,
                                                    if is_collapsed() { " collapsed" } else { "" },
                                                );
                                                view! {
                                                    <section class="plan-group">
                                                        <button
                                                            class=header_class
                                                            type="button"
                                                            on:click=toggle
                                                            aria-expanded=move || (!is_collapsed()).to_string()
                                                        >
                                                            <span class="plan-group__toggle">
                                                                {move || if is_collapsed() { "▸" } else { "▾" }}
                                                            </span>
                                                            <span class="plan-group__label">
                                                                {plan_group_label(status)}
                                                            </span>
                                                            <span class="plan-group__count">{count}</span>
                                                        </button>
                                                        <Show when=move || !is_collapsed() fallback=|| view! { <></> }>
                                                            {let group_nodes = group_nodes.clone(); move || {
                                                                group_nodes
                                                                    .clone()
                                                                    .into_iter()
                                                                    .map(|node| plan_node_view(node, 0, graph_refresh, runs_refresh))
                                                                    .collect_view()
                                                            }}
                                                        </Show>
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

    // ── Flow layout ──────────────────────────────────────────────────────────

    fn gnode(id: TaskId, position: u32, depends_on: Vec<TaskId>) -> api::PlanGraphNode {
        api::PlanGraphNode {
            task_id: id,
            position,
            depends_on,
            title: format!("t{position}"),
            status: daruma_domain::Status::Todo,
        }
    }

    fn gedge(from: TaskId, to: TaskId) -> api::PlanGraphEdge {
        api::PlanGraphEdge {
            from,
            to,
            kind: "depends_on".into(),
        }
    }

    #[test]
    fn flow_ranks_by_longest_chain_and_is_stable() {
        let (a, b, c, d) = (TaskId::new(), TaskId::new(), TaskId::new(), TaskId::new());
        // a -> b -> d, a -> c -> d: d must land behind both branches (rank 2),
        // not rank 1 next to c, or the arrow would point backwards.
        let nodes = vec![
            gnode(a, 0, vec![]),
            gnode(b, 1, vec![a]),
            gnode(c, 2, vec![a]),
            gnode(d, 3, vec![b, c]),
        ];
        let edges = vec![gedge(a, b), gedge(a, c), gedge(b, d), gedge(c, d)];

        let flow = plan_flow_layout(&nodes, &edges);
        let rank = |id: TaskId| flow.place(id).unwrap().rank;
        assert_eq!(rank(a), 0);
        assert_eq!(rank(b), 1);
        assert_eq!(rank(c), 1);
        assert_eq!(rank(d), 2);

        // Every edge runs strictly left to right — that is the whole promise of
        // the diagram.
        for e in &edges {
            assert!(rank(e.from) < rank(e.to), "edge went backwards");
        }

        // Same input, same picture: re-laying out must not move anything.
        assert_eq!(plan_flow_layout(&nodes, &edges), flow);
        // Input order must not matter either.
        let mut shuffled = nodes.clone();
        shuffled.reverse();
        assert_eq!(plan_flow_layout(&shuffled, &edges), flow);
    }

    #[test]
    fn independent_tasks_share_the_first_column_ordered_by_position() {
        let (a, b) = (TaskId::new(), TaskId::new());
        let nodes = vec![gnode(b, 5, vec![]), gnode(a, 1, vec![])];
        let flow = plan_flow_layout(&nodes, &[]);
        assert_eq!(flow.place(a).unwrap().rank, 0);
        assert_eq!(flow.place(b).unwrap().rank, 0);
        assert_eq!(flow.place(a).unwrap().row, 0);
        assert_eq!(flow.place(b).unwrap().row, 1);
    }

    #[test]
    fn a_cycle_does_not_hang_the_tab() {
        let (a, b) = (TaskId::new(), TaskId::new());
        let nodes = vec![gnode(a, 0, vec![b]), gnode(b, 1, vec![a])];
        let flow = plan_flow_layout(&nodes, &[gedge(a, b), gedge(b, a)]);
        assert_eq!(flow.boxes.len(), 2);
    }

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
