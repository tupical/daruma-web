//! Interactive WorkspaceGraph view — "what has formed" screen.
//!
//! Renders nodes (tasks/plans/documents/artifacts/projects/comments) and
//! edges (Contains, Blocks, Produces, ArtXxx, …) as an SVG with
//! force-directed layout, zoom/pan, click-to-details, impact mode, and
//! live incremental updates from the event store.
//!
//! # Layout
//!
//! A simple spring-repulsion force layout runs in `spawn_local` iterations
//! driven by `gloo_timers`.  No heavy JS deps — pure WASM.
//!
//! # Live updates
//!
//! Graph-relevant events arrive via `EventStoreCtx::graph_events`.  A
//! debounced effect (300 ms via `gloo_timers`) reconciles them against the
//! local `GraphNeighborhood` without a full HTTP re-fetch.

use std::collections::{HashMap, HashSet};

use gloo_timers::callback::Timeout;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::api::{
    self, GraphNeighborhood, GraphNode,
};
use crate::event_store::EventStoreCtx;
use crate::projects_ctx::ProjectsCtx;

// ── Constants ────────────────────────────────────────────────────────────────

const INITIAL_LIMIT: u32 = 80;
const IMPACT_LIMIT: u32 = 40;
const LAYOUT_ITERATIONS: u32 = 60;
const TICK_MS: u32 = 16; // ~60 fps during layout
const DEBOUNCE_MS: u32 = 300;

// SVG canvas
const SVG_W: f64 = 900.0;
const SVG_H: f64 = 620.0;

// Force params
const REPULSION: f64 = 4000.0;
const SPRING_LEN: f64 = 120.0;
const SPRING_K: f64 = 0.04;
const DAMPING: f64 = 0.85;
const CENTER_PULL: f64 = 0.002;

// ── Node kind helpers ────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Task,
    Plan,
    Project,
    Document,
    Artifact,
    Comment,
    Unknown,
}

impl NodeKind {
    fn from_str(s: &str) -> Self {
        match s {
            "Task" => NodeKind::Task,
            "Plan" => NodeKind::Plan,
            "Project" => NodeKind::Project,
            "Document" => NodeKind::Document,
            "Artifact" => NodeKind::Artifact,
            "Comment" => NodeKind::Comment,
            _ => NodeKind::Unknown,
        }
    }

    fn color(&self) -> &'static str {
        match self {
            NodeKind::Task => "#539bf5",     // blue
            NodeKind::Plan => "#b083f0",     // purple
            NodeKind::Project => "#3fb950",  // green
            NodeKind::Document => "#e3b341", // yellow
            NodeKind::Artifact => "#e09b44", // orange
            NodeKind::Comment => "#768390",  // gray
            NodeKind::Unknown => "#636e7b",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            NodeKind::Task => "Task",
            NodeKind::Plan => "Plan",
            NodeKind::Project => "Project",
            NodeKind::Document => "Doc",
            NodeKind::Artifact => "Artifact",
            NodeKind::Comment => "Comment",
            NodeKind::Unknown => "?",
        }
    }
}

// ── Edge kind helpers ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    Contains,
    PlanContains,
    ParentPlan,
    Blocks,
    RelatesTo,
    Duplicates,
    WasBlocking,
    CommentOn,
    Produces,
    ArtifactRel,
    Unknown,
}

impl EdgeKind {
    fn from_str(s: &str) -> Self {
        match s {
            "Contains" => EdgeKind::Contains,
            "PlanContains" => EdgeKind::PlanContains,
            "ParentPlan" => EdgeKind::ParentPlan,
            "Blocks" => EdgeKind::Blocks,
            "RelatesTo" => EdgeKind::RelatesTo,
            "Duplicates" => EdgeKind::Duplicates,
            "WasBlocking" => EdgeKind::WasBlocking,
            "CommentOn" => EdgeKind::CommentOn,
            "Produces" => EdgeKind::Produces,
            s if s.starts_with("Art") => EdgeKind::ArtifactRel,
            _ => EdgeKind::Unknown,
        }
    }

    fn stroke_color(&self) -> &'static str {
        match self {
            EdgeKind::Blocks => "#f47067",
            EdgeKind::Produces | EdgeKind::ArtifactRel => "#e09b44",
            EdgeKind::RelatesTo | EdgeKind::Duplicates => "#768390",
            EdgeKind::CommentOn => "#636e7b",
            _ => "#3d444d",
        }
    }

    fn stroke_dash(&self) -> &'static str {
        match self {
            EdgeKind::Blocks | EdgeKind::RelatesTo | EdgeKind::Duplicates | EdgeKind::WasBlocking => {
                "5,4"
            }
            EdgeKind::Produces | EdgeKind::ArtifactRel => "8,3",
            _ => "none",
        }
    }

    fn stroke_width(&self) -> f64 {
        match self {
            EdgeKind::Blocks => 1.8,
            EdgeKind::Contains | EdgeKind::PlanContains => 1.2,
            _ => 1.0,
        }
    }
}

// ── Layout position ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct NodePos {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    pinned: bool,
}

// ── Component state ───────────────────────────────────────────────────────────

/// Tracks which node kinds and edge kinds are visible.
#[derive(Clone, Debug)]
struct FilterState {
    hidden_node_kinds: HashSet<NodeKind>,
    hidden_edge_kinds: HashSet<EdgeKind>,
}

impl FilterState {
    fn new() -> Self {
        Self {
            hidden_node_kinds: HashSet::new(),
            hidden_edge_kinds: HashSet::new(),
        }
    }

    fn node_visible(&self, kind: &NodeKind) -> bool {
        !self.hidden_node_kinds.contains(kind)
    }

    fn edge_visible(&self, kind: &EdgeKind) -> bool {
        !self.hidden_edge_kinds.contains(kind)
    }
}

// ── WorkspaceGraph component ──────────────────────────────────────────────────

/// Fetch the graph for a list of project ids (formatted as `prj_<uuid>`),
/// calling `workspacegraph_related` for each root and merging results.
/// Deduplicates nodes by `id`; edges are included if both endpoints are present.
async fn fetch_full_neighborhood(project_source_ids: Vec<String>) -> Option<GraphNeighborhood> {
    if project_source_ids.is_empty() {
        return None;
    }
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut seen_nodes: HashSet<String> = HashSet::new();
    let mut all_edges = Vec::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();

    for source_id in &project_source_ids {
        // node_id format the server expects: "project:prj_<uuid>"
        let node_id = format!("project:{source_id}");
        match api::workspacegraph_related(&node_id, 2, INITIAL_LIMIT).await {
            Ok(nb) => {
                for node in nb.nodes {
                    if seen_nodes.insert(node.id.clone()) {
                        all_nodes.push(node);
                    }
                }
                for edge in nb.edges {
                    let key = (edge.from_id.clone(), edge.to_id.clone(), edge.kind.clone());
                    if seen_edges.insert(key) {
                        all_edges.push(edge);
                    }
                }
            }
            Err(e) => {
                leptos::logging::warn!(
                    "[graph] related({node_id}) failed: {e}"
                );
            }
        }
    }

    Some(GraphNeighborhood {
        nodes: all_nodes,
        edges: all_edges,
    })
}

#[component]
pub fn WorkspaceGraph() -> impl IntoView {
    // Event store for live updates.
    let store = use_context::<EventStoreCtx>().expect("EventStoreCtx");
    // Projects context — provides real project ids for bootstrap.
    let projects_ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");

    // Graph data — neighborhood fetched from server.
    let neighborhood: RwSignal<Option<GraphNeighborhood>> = RwSignal::new(None);
    let loading = RwSignal::new(true);
    let load_error: RwSignal<Option<String>> = RwSignal::new(None);

    // Force-layout positions: node_id → NodePos.
    let positions: RwSignal<HashMap<String, NodePos>> = RwSignal::new(HashMap::new());

    // Layout running flag (prevents overlapping ticks).
    let layout_running = RwSignal::new(false);

    // Selected node for the details panel.
    let selected_node: RwSignal<Option<GraphNode>> = RwSignal::new(None);

    // Impact mode: highlighted node ids.
    let impact_ids: RwSignal<HashSet<String>> = RwSignal::new(HashSet::new());
    let impact_mode = RwSignal::new(false);

    // Filters.
    let filter_state: RwSignal<FilterState> = RwSignal::new(FilterState::new());

    // Zoom/pan: (scale, tx, ty).
    let zoom: RwSignal<(f64, f64, f64)> = RwSignal::new((1.0, 0.0, 0.0));

    // Cursor applied from graph_events.
    let event_cursor: RwSignal<usize> = RwSignal::new(0);

    // Bootstrap-started flag: prevents re-running the initial fetch when the
    // projects signal updates for other reasons after we've already loaded.
    let bootstrap_done = RwSignal::new(false);

    // ── Initial fetch ──────────────────────────────────────────────────────
    // Reactive on `projects` — fires once projects are non-empty, then stops.

    Effect::new(move |_| {
        // Already bootstrapped — ignore subsequent project-signal updates.
        if bootstrap_done.get_untracked() {
            return;
        }

        let projects = projects_ctx.projects.get(); // reactive dependency

        let project_source_ids: Vec<String> = projects
            .iter()
            .map(|p| p.id.to_string())
            .collect();

        if project_source_ids.is_empty() {
            // Projects not loaded yet — wait for next reactive tick.
            return;
        }

        // Mark done before spawning so a second effect-fire won't double-fetch.
        bootstrap_done.set(true);

        spawn_local(async move {
            loading.set(true);
            match fetch_full_neighborhood(project_source_ids).await {
                Some(nb) if !nb.nodes.is_empty() => {
                    init_positions(&nb, positions);
                    neighborhood.set(Some(nb));
                    loading.set(false);
                    run_layout(positions, layout_running, LAYOUT_ITERATIONS);
                }
                Some(_empty) => {
                    neighborhood.set(Some(GraphNeighborhood {
                        nodes: vec![],
                        edges: vec![],
                    }));
                    loading.set(false);
                }
                None => {
                    load_error.set(Some(
                        "No projects found — cannot load workspace graph.".into(),
                    ));
                    loading.set(false);
                }
            }
        });
    });

    // ── Debounced live update from graph_events ────────────────────────────

    Effect::new(move |_| {
        let len = store.graph_events.with(|v| v.len());
        let start = event_cursor.get_untracked();
        if start >= len {
            return;
        }

        // Snapshot project ids now (before the async boundary) so the Timeout
        // closure captures a plain Vec<String> — not the ProjectsCtx signal,
        // which would make the closure FnOnce.
        let project_source_ids: Vec<String> = projects_ctx
            .projects
            .get_untracked()
            .iter()
            .map(|p| p.id.to_string())
            .collect();

        let _t = Timeout::new(DEBOUNCE_MS, move || {
            let current_len = store.graph_events.with_untracked(|v| v.len());
            event_cursor.set(current_len);

            spawn_local(async move {
                if let Some(nb) = fetch_full_neighborhood(project_source_ids).await {
                    positions.update(|m| {
                        for node in &nb.nodes {
                            if !m.contains_key(&node.id) {
                                let offset = (m.len() as f64 * 37.0).sin() * 60.0;
                                m.insert(
                                    node.id.clone(),
                                    NodePos {
                                        x: SVG_W / 2.0 + offset,
                                        y: SVG_H / 2.0 + offset * 0.6,
                                        vx: 0.0,
                                        vy: 0.0,
                                        pinned: false,
                                    },
                                );
                            }
                        }
                        let ids: HashSet<&String> = nb.nodes.iter().map(|n| &n.id).collect();
                        m.retain(|k, _| ids.contains(k));
                    });
                    neighborhood.set(Some(nb));
                    run_layout(positions, layout_running, 20);
                }
            });
        });
        _t.forget();
    });

    // ── Render ─────────────────────────────────────────────────────────────

    view! {
        <div class="workspace-graph-container">
            <div class="workspace-graph-toolbar">
                <span class="workspace-graph-toolbar__title">"Workspace Graph"</span>
                <GraphFilters filter_state=filter_state />
                <button
                    class="btn-ghost btn-sm"
                    type="button"
                    title="Reset zoom"
                    on:click=move |_| zoom.set((1.0, 0.0, 0.0))
                >
                    "⊞ Reset"
                </button>
                <Show when=move || impact_mode.get()>
                    <button
                        class="btn-ghost btn-sm btn-impact"
                        type="button"
                        on:click=move |_| {
                            impact_mode.set(false);
                            impact_ids.set(HashSet::new());
                        }
                    >
                        "Exit Impact"
                    </button>
                </Show>
            </div>

            <div class="workspace-graph-body">
                {move || {
                    if loading.get() {
                        return view! {
                            <div class="workspace-graph-loading">"Loading graph…"</div>
                        }.into_any();
                    }
                    if let Some(err) = load_error.get() {
                        return view! {
                            <div class="workspace-graph-error">{err}</div>
                        }.into_any();
                    }
                    let is_empty = neighborhood.with(|nb| {
                        nb.as_ref().map(|n| n.nodes.is_empty()).unwrap_or(true)
                    });
                    if is_empty {
                        return view! {
                            <div class="workspace-graph-empty">
                                <span>"No graph data yet."</span>
                                <span class="workspace-graph-empty__hint">
                                    "Create tasks, plans or documents to see them here."
                                </span>
                            </div>
                        }.into_any();
                    }
                    view! {
                        <>
                            <GraphSvg
                                neighborhood=neighborhood
                                positions=positions
                                filter_state=filter_state
                                selected_node=selected_node
                                impact_ids=impact_ids
                                impact_mode=impact_mode
                                zoom=zoom
                            />
                            <Show when=move || selected_node.get().is_some()>
                                <NodeDetailsPanel
                                    node=selected_node
                                    impact_ids=impact_ids
                                    impact_mode=impact_mode
                                />
                            </Show>
                        </>
                    }.into_any()
                }}
            </div>
        </div>
    }
}

// ── GraphSvg ─────────────────────────────────────────────────────────────────

#[component]
fn GraphSvg(
    neighborhood: RwSignal<Option<GraphNeighborhood>>,
    positions: RwSignal<HashMap<String, NodePos>>,
    filter_state: RwSignal<FilterState>,
    selected_node: RwSignal<Option<GraphNode>>,
    impact_ids: RwSignal<HashSet<String>>,
    impact_mode: RwSignal<bool>,
    zoom: RwSignal<(f64, f64, f64)>,
) -> impl IntoView {
    // Mouse drag state stored in signals (WASM single-thread; no Mutex needed).
    let dragging: RwSignal<Option<(f64, f64, f64, f64)>> = RwSignal::new(None); // (start_mx, start_my, start_tx, start_ty)

    let on_mousedown = move |e: web_sys::MouseEvent| {
        let (_, tx, ty) = zoom.get_untracked();
        dragging.set(Some((e.client_x() as f64, e.client_y() as f64, tx, ty)));
    };

    let on_mousemove = move |e: web_sys::MouseEvent| {
        if let Some((sx, sy, stx, sty)) = dragging.get_untracked() {
            let dx = e.client_x() as f64 - sx;
            let dy = e.client_y() as f64 - sy;
            let (sc, _, _) = zoom.get_untracked();
            zoom.set((sc, stx + dx, sty + dy));
        }
    };

    let on_mouseup = move |_: web_sys::MouseEvent| {
        dragging.set(None);
    };

    let on_wheel = move |e: web_sys::WheelEvent| {
        e.prevent_default();
        let delta = e.delta_y();
        let factor = if delta < 0.0 { 1.12 } else { 1.0 / 1.12 };

        // Zoom relative to the cursor position so the point under the pointer
        // stays fixed.  Standard zoom-to-cursor formula:
        //   world_x = (cursor_x - tx) / sc
        //   new_sc  = clamp(sc * factor)
        //   new_tx  = cursor_x - world_x * new_sc
        //             = cursor_x - (cursor_x - tx) * (new_sc / sc)
        //
        // WheelEvent inherits MouseEvent; offset_x/y give coordinates relative
        // to the target element's padding edge — exactly what we need for SVG
        // viewport math, and requires no extra web-sys features.
        let cx = e.offset_x() as f64;
        let cy = e.offset_y() as f64;

        zoom.update(|(sc, tx, ty)| {
            let new_sc = (*sc * factor).clamp(0.1, 8.0);
            let ratio = new_sc / *sc;
            *tx = cx - (cx - *tx) * ratio;
            *ty = cy - (cy - *ty) * ratio;
            *sc = new_sc;
        });
    };

    view! {
        <svg
            class="workspace-graph-svg"
            width=SVG_W.to_string()
            height=SVG_H.to_string()
            viewBox=move || format!("0 0 {} {}", SVG_W, SVG_H)
            on:mousedown=on_mousedown
            on:mousemove=on_mousemove
            on:mouseup=on_mouseup
            on:mouseleave=on_mouseup
            on:wheel=on_wheel
            style="cursor: grab; display: block;"
        >
            <defs>
                <marker
                    id="arrowhead"
                    markerWidth="8"
                    markerHeight="6"
                    refX="8"
                    refY="3"
                    orient="auto"
                >
                    <polygon points="0 0, 8 3, 0 6" fill="#768390" />
                </marker>
            </defs>

            <g transform=move || {
                let (sc, tx, ty) = zoom.get();
                format!("translate({tx},{ty}) scale({sc})")
            }>
                // Edges layer
                <GraphEdges
                    neighborhood=neighborhood
                    positions=positions
                    filter_state=filter_state
                    impact_ids=impact_ids
                    impact_mode=impact_mode
                />
                // Nodes layer
                <GraphNodes
                    neighborhood=neighborhood
                    positions=positions
                    filter_state=filter_state
                    selected_node=selected_node
                    impact_ids=impact_ids
                    impact_mode=impact_mode
                />
            </g>
        </svg>
    }
}

// ── GraphEdges ────────────────────────────────────────────────────────────────

#[component]
fn GraphEdges(
    neighborhood: RwSignal<Option<GraphNeighborhood>>,
    positions: RwSignal<HashMap<String, NodePos>>,
    filter_state: RwSignal<FilterState>,
    impact_ids: RwSignal<HashSet<String>>,
    impact_mode: RwSignal<bool>,
) -> impl IntoView {
    let edges = move || {
        let nb = neighborhood.get();
        let pos = positions.get();
        let fs = filter_state.get();
        let iids = impact_ids.get();
        let in_impact = impact_mode.get();

        let Some(nb) = nb else {
            return vec![];
        };

        nb.edges
            .iter()
            .filter_map(|edge| {
                let ek = EdgeKind::from_str(&edge.kind);
                if !fs.edge_visible(&ek) {
                    return None;
                }
                let from = pos.get(&edge.from_id)?;
                let to = pos.get(&edge.to_id)?;

                let dimmed = in_impact
                    && !iids.contains(&edge.from_id)
                    && !iids.contains(&edge.to_id);
                let opacity = if dimmed { 0.12 } else { 0.7 };
                let color = ek.stroke_color();
                let dash = ek.stroke_dash();
                let width = ek.stroke_width();

                // Shorten line to not overlap node circles.
                let (x1, y1, x2, y2) = shorten_line(from.x, from.y, to.x, to.y, 14.0, 16.0);

                Some(view! {
                    <line
                        x1=x1.to_string() y1=y1.to_string()
                        x2=x2.to_string() y2=y2.to_string()
                        stroke=color
                        stroke-width=width.to_string()
                        stroke-dasharray=dash
                        stroke-opacity=opacity.to_string()
                        marker-end="url(#arrowhead)"
                    />
                })
            })
            .collect::<Vec<_>>()
    };

    view! {
        <g class="graph-edges">{edges}</g>
    }
}

// ── GraphNodes ────────────────────────────────────────────────────────────────

#[component]
fn GraphNodes(
    neighborhood: RwSignal<Option<GraphNeighborhood>>,
    positions: RwSignal<HashMap<String, NodePos>>,
    filter_state: RwSignal<FilterState>,
    selected_node: RwSignal<Option<GraphNode>>,
    impact_ids: RwSignal<HashSet<String>>,
    impact_mode: RwSignal<bool>,
) -> impl IntoView {
    let nodes = move || {
        let nb = neighborhood.get();
        let pos = positions.get();
        let fs = filter_state.get();
        let iids = impact_ids.get();
        let in_impact = impact_mode.get();
        let sel = selected_node.get();

        let Some(nb) = nb else {
            return vec![];
        };

        nb.nodes
            .iter()
            .filter_map(|node| {
                let nk = NodeKind::from_str(&node.kind);
                if !fs.node_visible(&nk) {
                    return None;
                }
                let p = pos.get(&node.id)?;

                let dimmed = in_impact && !iids.contains(&node.id);
                let is_selected = sel.as_ref().map(|s| s.id == node.id).unwrap_or(false);
                let is_impact_root = in_impact && iids.iter().next().map(|id| id == &node.id).unwrap_or(false);

                let color = nk.color();
                let label = truncate_title(&node.title, 18);
                let cx = p.x;
                let cy = p.y;
                let node_clone = node.clone();

                Some(view! {
                    <g
                        class="graph-node"
                        transform=format!("translate({cx},{cy})")
                        style="cursor: pointer;"
                        on:click=move |_| selected_node.set(Some(node_clone.clone()))
                    >
                        <NodeShape
                            kind=nk.clone()
                            color=color.to_string()
                            selected=is_selected
                            dimmed=dimmed
                            impact_root=is_impact_root
                        />
                        <text
                            x="0" y="22"
                            text-anchor="middle"
                            font-size="9"
                            fill="#cdd9e5"
                            opacity=if dimmed { "0.3" } else { "0.85" }
                            style="pointer-events: none; user-select: none;"
                        >
                            {label}
                        </text>
                    </g>
                })
            })
            .collect::<Vec<_>>()
    };

    view! {
        <g class="graph-nodes">{nodes}</g>
    }
}

// ── NodeShape ─────────────────────────────────────────────────────────────────

#[component]
fn NodeShape(
    kind: NodeKind,
    color: String,
    selected: bool,
    dimmed: bool,
    impact_root: bool,
) -> impl IntoView {
    let opacity = if dimmed { "0.25" } else { "1" };
    let stroke_color = if selected || impact_root {
        "#f0f6fc".to_string()
    } else {
        color.clone()
    };
    let stroke_width = if selected || impact_root { "2.5" } else { "1.2" };

    match kind {
        NodeKind::Task => view! {
            // Circle
            <circle r="11" fill=color stroke=stroke_color stroke-width=stroke_width opacity=opacity />
        }.into_any(),

        NodeKind::Plan => view! {
            // Diamond
            <polygon
                points="0,-13 11,0 0,13 -11,0"
                fill=color stroke=stroke_color stroke-width=stroke_width opacity=opacity
            />
        }.into_any(),

        NodeKind::Project => view! {
            // Hexagon
            <polygon
                points="12,0 6,10 -6,10 -12,0 -6,-10 6,-10"
                fill=color stroke=stroke_color stroke-width=stroke_width opacity=opacity
            />
        }.into_any(),

        NodeKind::Document => view! {
            // Rectangle
            <rect x="-10" y="-8" width="20" height="16"
                rx="2"
                fill=color stroke=stroke_color stroke-width=stroke_width opacity=opacity
            />
        }.into_any(),

        NodeKind::Artifact => view! {
            // 5-pointed star
            <polygon
                points="0,-13 3,-5 12,-5 5,1 7,10 0,5 -7,10 -5,1 -12,-5 -3,-5"
                fill=color stroke=stroke_color stroke-width=stroke_width opacity=opacity
            />
        }.into_any(),

        NodeKind::Comment | NodeKind::Unknown => view! {
            // Small circle
            <circle r="7" fill=color stroke=stroke_color stroke-width=stroke_width opacity=opacity />
        }.into_any(),
    }
}

// ── Node details panel ────────────────────────────────────────────────────────

#[component]
fn NodeDetailsPanel(
    node: RwSignal<Option<GraphNode>>,
    impact_ids: RwSignal<HashSet<String>>,
    impact_mode: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <div class="node-details-panel">
            {move || {
                let Some(n) = node.get() else { return view! { <></> }.into_any(); };
                let nk = NodeKind::from_str(&n.kind);
                let node_id_for_impact = n.id.clone();
                let title = n.title.clone();
                let text = n.text.clone();
                let source_id = n.source_id.clone();
                let project_id = n.project_id.clone();
                let has_text = !text.is_empty();
                let has_project = project_id.is_some();
                let bg_color = format!("background: {}", nk.color());
                let kind_label = nk.label();
                view! {
                    <div class="node-details-panel__inner">
                        <div class="node-details-panel__header">
                            <span
                                class="node-details-panel__kind-badge"
                                style=bg_color
                            >
                                {kind_label}
                            </span>
                            <span class="node-details-panel__title">{title}</span>
                            <button
                                type="button"
                                class="node-details-panel__close"
                                on:click=move |_| node.set(None)
                            >"×"</button>
                        </div>

                        <Show when=move || has_text>
                            <p class="node-details-panel__text">{text.clone()}</p>
                        </Show>

                        <div class="node-details-panel__meta">
                            <span class="node-details-panel__meta-item">
                                <span class="node-details-panel__meta-label">"ID: "</span>
                                {source_id}
                            </span>
                            <Show when=move || has_project>
                                <span class="node-details-panel__meta-item">
                                    <span class="node-details-panel__meta-label">"Project: "</span>
                                    {project_id.clone().unwrap_or_default()}
                                </span>
                            </Show>
                        </div>

                        <button
                            type="button"
                            class="btn-ghost btn-sm"
                            on:click=move |_| {
                                let nid = node_id_for_impact.clone();
                                spawn_local(async move {
                                    if let Ok(nb) = api::workspacegraph_impact(&nid, IMPACT_LIMIT).await {
                                        let ids: HashSet<String> = nb.nodes.iter()
                                            .map(|nd| nd.id.clone())
                                            .collect();
                                        impact_ids.set(ids);
                                        impact_mode.set(true);
                                    }
                                });
                            }
                        >
                            "Show Impact"
                        </button>
                    </div>
                }.into_any()
            }}
        </div>
    }
}

// ── Filters bar ───────────────────────────────────────────────────────────────

#[component]
fn GraphFilters(filter_state: RwSignal<FilterState>) -> impl IntoView {
    let all_node_kinds = [
        NodeKind::Task,
        NodeKind::Plan,
        NodeKind::Project,
        NodeKind::Document,
        NodeKind::Artifact,
        NodeKind::Comment,
    ];

    let chips = all_node_kinds.iter().map(|kind| {
        let kind = kind.clone();
        let color = kind.color();
        let label = kind.label();
        let hidden_kind = kind.clone();
        let toggle_kind = kind.clone();
        let is_hidden = move || filter_state.with(|f| f.hidden_node_kinds.contains(&hidden_kind));
        view! {
            <button
                type="button"
                class="graph-filter-chip"
                style=move || format!(
                    "border-color: {}; opacity: {};",
                    color,
                    if is_hidden() { "0.35" } else { "1.0" }
                )
                on:click=move |_| {
                    filter_state.update(|f| {
                        if !f.hidden_node_kinds.remove(&toggle_kind) {
                            f.hidden_node_kinds.insert(toggle_kind.clone());
                        }
                    });
                }
            >
                <span style=format!("color: {color}")>{label}</span>
            </button>
        }
    }).collect_view();

    view! {
        <div class="graph-filter-bar">
            {chips}
        </div>
    }
}

// ── Force layout ──────────────────────────────────────────────────────────────

/// Initialise positions in a circle for all nodes in `nb`.
fn init_positions(nb: &GraphNeighborhood, positions: RwSignal<HashMap<String, NodePos>>) {
    let n = nb.nodes.len().max(1);
    positions.update(|m| {
        m.clear();
        for (i, node) in nb.nodes.iter().enumerate() {
            let angle = (i as f64 / n as f64) * std::f64::consts::TAU;
            let r = (n as f64 * 12.0).min(240.0).max(80.0);
            m.insert(
                node.id.clone(),
                NodePos {
                    x: SVG_W / 2.0 + r * angle.cos(),
                    y: SVG_H / 2.0 + r * angle.sin(),
                    vx: 0.0,
                    vy: 0.0,
                    pinned: false,
                },
            );
        }
    });
}

/// Run `steps` force-layout ticks, one per TICK_MS, updating `positions`.
fn run_layout(
    positions: RwSignal<HashMap<String, NodePos>>,
    running: RwSignal<bool>,
    steps: u32,
) {
    if running.get_untracked() {
        return;
    }
    running.set(true);

    let counter = std::rc::Rc::new(std::cell::Cell::new(0u32));
    schedule_tick(positions, running, counter, steps);
}

fn schedule_tick(
    positions: RwSignal<HashMap<String, NodePos>>,
    running: RwSignal<bool>,
    counter: std::rc::Rc<std::cell::Cell<u32>>,
    max_steps: u32,
) {
    let counter_clone = counter.clone();
    let _t = Timeout::new(TICK_MS, move || {
        let step = counter_clone.get();
        if step >= max_steps {
            running.set(false);
            return;
        }
        positions.update(|m| force_tick(m));
        counter_clone.set(step + 1);
        schedule_tick(positions, running, counter_clone, max_steps);
    });
    _t.forget();
}

/// One step of spring-repulsion force layout.
fn force_tick(pos: &mut HashMap<String, NodePos>) {
    let ids: Vec<String> = pos.keys().cloned().collect();
    let snapshots: Vec<(String, f64, f64)> = ids
        .iter()
        .map(|id| {
            let p = &pos[id];
            (id.clone(), p.x, p.y)
        })
        .collect();

    // Compute forces.
    let mut forces: HashMap<String, (f64, f64)> = ids
        .iter()
        .map(|id| (id.clone(), (0.0f64, 0.0f64)))
        .collect();

    // Repulsion between all pairs.
    for i in 0..snapshots.len() {
        for j in (i + 1)..snapshots.len() {
            let (ref ai, xi, yi) = snapshots[i];
            let (ref aj, xj, yj) = snapshots[j];
            let dx = xi - xj;
            let dy = yi - yj;
            let dist2 = (dx * dx + dy * dy).max(1.0);
            let dist = dist2.sqrt();
            let force = REPULSION / dist2;
            let fx = force * dx / dist;
            let fy = force * dy / dist;
            if let Some(f) = forces.get_mut(ai) {
                f.0 += fx;
                f.1 += fy;
            }
            if let Some(f) = forces.get_mut(aj) {
                f.0 -= fx;
                f.1 -= fy;
            }
        }
    }

    // Centre gravity.
    for (id, x, y) in &snapshots {
        let dx = SVG_W / 2.0 - x;
        let dy = SVG_H / 2.0 - y;
        if let Some(f) = forces.get_mut(id) {
            f.0 += dx * CENTER_PULL;
            f.1 += dy * CENTER_PULL;
        }
    }

    // Apply.
    for id in &ids {
        let node = pos.get_mut(id).unwrap();
        if node.pinned {
            continue;
        }
        let (fx, fy) = forces[id];
        node.vx = (node.vx + fx) * DAMPING;
        node.vy = (node.vy + fy) * DAMPING;
        node.x = (node.x + node.vx).clamp(20.0, SVG_W - 20.0);
        node.y = (node.y + node.vy).clamp(20.0, SVG_H - 20.0);
    }
}

// ── Geometry helpers ───────────────────────────────────────────────────────────

/// Shorten a line segment so it doesn't overlap node shapes.
fn shorten_line(
    x1: f64, y1: f64,
    x2: f64, y2: f64,
    r1: f64, r2: f64,
) -> (f64, f64, f64, f64) {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let ux = dx / len;
    let uy = dy / len;
    (
        x1 + ux * r1,
        y1 + uy * r1,
        x2 - ux * r2,
        y2 - uy * r2,
    )
}

/// Truncate a title to `max_chars` with ellipsis.
fn truncate_title(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[..max_chars - 1].iter().collect::<String>() + "…"
    }
}
