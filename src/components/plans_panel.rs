use crate::api;
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::ws::WsCtx;
use leptos::prelude::*;
use std::collections::HashMap;
use daruma_domain::{Plan, PlanPatch, PlanStatus};
use daruma_events::{Event, EventEnvelope};
use daruma_shared::time::Timestamp;
use wasm_bindgen_futures::spawn_local;

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

// ── Treeview renderer ─────────────────────────────────────────────────────────
//
// Plain function (not #[component]) so it can recurse without type-system issues.
// Returns AnyView for uniform type across recursion levels.

fn plan_node_view(node: PlanTreeNode, depth: usize) -> AnyView {
    let has_children = !node.children.is_empty();
    let plan = node.plan;
    let expanded = RwSignal::new(true);

    // Render children eagerly; toggling is done via CSS display:none/block only.
    let children_views: Vec<AnyView> = node
        .children
        .into_iter()
        .map(|child| plan_node_view(child, depth + 1))
        .collect();

    let title = plan.title.clone();
    let status = plan.status;
    let criteria_count = plan.success_criteria.len();
    let sc = status_class(&status);
    let sl = status_label(&status);
    let is_abandoned = status == PlanStatus::Abandoned || plan.archived_at.is_some();

    // Inline CSS custom property drives depth-based indent in stylesheet:
    //   padding-left: calc(var(--depth, 0) * 1rem + 0.6rem)
    let depth_style = format!("--depth:{depth}");

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
            </div>
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
        if cache.with_untracked(|m| m.contains_key(&pid)) {
            return;
        }
        let snapshot_at = ws_events.with_untracked(|v| v.len());
        let my_seq = fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0)) + 1;
        fetch_seq.update(|m| {
            m.insert(pid.clone(), my_seq);
        });

        spawn_local(async move {
            let mut ps = api::list_plans(&pid).await.unwrap_or_default();
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
                                    if ps.is_empty() {
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
                                                    .map(|node| plan_node_view(node, 0))
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
