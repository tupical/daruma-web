//! Composition map — the workspace by containment, one level at a time.
//!
//! Replaces the force-directed workspace graph. That view drew every node of
//! every project at once and let a spring simulation decide where things go;
//! the two relations that carry meaning here (a plan *contains* tasks, a task
//! *depends on* another) both have a canonical geometry, and free 2D layout
//! destroys both. Containment is nesting, dependency is order.
//!
//! So: projects hold plans, a plan expands in place into its flow diagram
//! (`plans_panel::render_plan_graph`, columns = dependency ranks). Nothing is
//! drawn until you ask for it, and every position is derived from the data.
//!
//! # What this deliberately does not draw
//!
//! Arrows between plans. Collapsing task→task edges into plan→plan ones needs a
//! task→plan map, and the workspace-graph API only carries `project_id` on a
//! node — building it would cost one request per plan. Rather than draw a
//! partial dependency picture that looks complete, the map says so in the
//! header and leaves the arrows to the expanded flow.

use crate::api;
use crate::components::plans_panel::{render_plan_graph, spawn_graph_fetch, PlanGraphBundle};
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use daruma_domain::{Plan, PlanStatus, Project};
use leptos::prelude::*;

/// Projects whose plans the map shows, honouring the project bar.
fn projects_in_scope(filter: &ProjectFilter, projects: &[Project]) -> Vec<Project> {
    match filter {
        ProjectFilter::Of(pid) => projects.iter().filter(|p| p.id == *pid).cloned().collect(),
        // Inbox holds project-less tasks, which by definition have no plans.
        ProjectFilter::Inbox => Vec::new(),
        ProjectFilter::All => projects.to_vec(),
    }
}

/// Active first, then draft; finished work sinks. Ties by title so the order is
/// stable across loads rather than however the server happened to return them.
fn sort_plans(plans: &mut [Plan]) {
    fn rank(s: PlanStatus) -> u8 {
        match s {
            PlanStatus::Active => 0,
            PlanStatus::Draft => 1,
            PlanStatus::Completed => 2,
            PlanStatus::Abandoned => 3,
        }
    }
    plans.sort_by(|a, b| {
        rank(a.status)
            .cmp(&rank(b.status))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
}

#[component]
pub fn CompositionMap() -> impl IntoView {
    let ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");

    let scope =
        Memo::new(move |_| projects_in_scope(&ctx.current_filter.get(), &ctx.projects.get()));

    view! {
        <div class="composition-map">
            <div class="composition-map__header">
                <span class="composition-map__title">"Composition"</span>
                <span class="composition-map__hint">
                    "Projects → plans → tasks. Open a plan to see its dependency flow."
                </span>
            </div>

            <Show
                when=move || !scope.get().is_empty()
                fallback=move || view! {
                    <p class="composition-map__empty">
                        {move || match ctx.current_filter.get() {
                            ProjectFilter::Inbox => "Inbox tasks belong to no plan.".to_string(),
                            _ => "No projects yet.".to_string(),
                        }}
                    </p>
                }
            >
                <For each=move || scope.get() key=|p| p.id let:project>
                    <ProjectSection project=project />
                </For>
            </Show>
        </div>
    }
}

/// One project and its plans. The plan list is fetched once per project, on
/// mount — a plan's tasks wait until that plan is opened.
#[component]
fn ProjectSection(project: Project) -> impl IntoView {
    let title = project.title.clone();
    let project_id = project.id.to_string();
    let plans: RwSignal<Option<Result<Vec<Plan>, String>>> = RwSignal::new(None);

    {
        let project_id = project_id.clone();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let loaded = api::list_plans(&project_id)
                .await
                .map(|mut ps| {
                    sort_plans(&mut ps);
                    ps
                })
                .map_err(|e| e.friendly());
            plans.set(Some(loaded));
        });
    }

    view! {
        <section class="composition-project">
            <div class="composition-project__header">
                <span class="composition-project__title">{title}</span>
                <span class="composition-project__count">
                    {move || match plans.get() {
                        Some(Ok(ps)) => format!("{} plans", ps.len()),
                        Some(Err(_)) => String::new(),
                        None => "…".to_string(),
                    }}
                </span>
            </div>
            {move || match plans.get() {
                None => view! { <p class="composition-project__loading">"Loading…"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="fetch-error__message">{e}</p> }.into_any(),
                Some(Ok(ps)) if ps.is_empty() => {
                    view! { <p class="composition-project__loading">"No plans."</p> }.into_any()
                }
                Some(Ok(ps)) => view! {
                    <div class="composition-plans">
                        <For each=move || ps.clone() key=|p| p.id let:plan>
                            <PlanCard plan=plan />
                        </For>
                    </div>
                }
                .into_any(),
            }}
        </section>
    }
}

/// A collapsed plan is a box; opening it swaps in the flow diagram in place, so
/// the rest of the map does not move.
#[component]
fn PlanCard(plan: Plan) -> impl IntoView {
    let plan_id = plan.id.to_string();
    let title = plan.title.clone();
    let status = plan.status;
    let open = RwSignal::new(false);
    let data: RwSignal<Option<Result<PlanGraphBundle, String>>> = RwSignal::new(None);

    let on_toggle = {
        let plan_id = plan_id.clone();
        move |_: web_sys::MouseEvent| {
            open.update(|v| *v = !*v);
            if open.get_untracked() && data.get_untracked().is_none() {
                spawn_graph_fetch(plan_id.clone(), data);
            }
        }
    };

    let card_class = format!("composition-plan composition-plan--{}", status_slug(status));

    view! {
        <div class=card_class class:composition-plan--open=move || open.get()>
            <button class="composition-plan__head" type="button" on:click=on_toggle>
                <span class="composition-plan__chevron">
                    {move || if open.get() { "▾" } else { "▸" }}
                </span>
                <span class="composition-plan__title">{title}</span>
                <span class="composition-plan__status">{status_label(status)}</span>
            </button>
            <Show when=move || open.get() fallback=|| view! { <></> }>
                <div class="composition-plan__body">
                    {move || match data.get() {
                        None => view! { <p class="composition-project__loading">"Loading…"</p> }.into_any(),
                        Some(Err(e)) => view! { <p class="fetch-error__message">{e}</p> }.into_any(),
                        Some(Ok(bundle)) => render_plan_graph(&bundle),
                    }}
                </div>
            </Show>
        </div>
    }
}

fn status_slug(s: PlanStatus) -> &'static str {
    match s {
        PlanStatus::Draft => "draft",
        PlanStatus::Active => "active",
        PlanStatus::Completed => "completed",
        PlanStatus::Abandoned => "abandoned",
    }
}

fn status_label(s: PlanStatus) -> &'static str {
    match s {
        PlanStatus::Draft => "draft",
        PlanStatus::Active => "active",
        PlanStatus::Completed => "completed",
        PlanStatus::Abandoned => "abandoned",
    }
}

#[cfg(test)]
mod tests {
    use super::{projects_in_scope, sort_plans};
    use crate::projects_ctx::ProjectFilter;
    use daruma_domain::{Actor, Plan, PlanStatus, Project};
    use daruma_shared::{time, PlanId, ProjectId};

    fn plan(title: &str, status: PlanStatus, project_id: ProjectId) -> Plan {
        let now = time::now();
        Plan {
            id: PlanId::new(),
            project_id,
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
        }
    }

    #[test]
    fn scope_follows_the_project_bar() {
        let one = Project::new("one", None);
        let two = Project::new("two", None);
        let all = vec![one.clone(), two.clone()];

        assert_eq!(projects_in_scope(&ProjectFilter::All, &all).len(), 2);
        assert_eq!(
            projects_in_scope(&ProjectFilter::Of(two.id), &all)
                .first()
                .map(|p| p.id),
            Some(two.id)
        );
        // Inbox is the project-less bucket — nothing there can belong to a plan.
        assert!(projects_in_scope(&ProjectFilter::Inbox, &all).is_empty());
    }

    #[test]
    fn plans_sort_active_first_and_deterministically() {
        let pid = Project::new("p", None).id;
        let mut plans = vec![
            plan("zeta", PlanStatus::Completed, pid),
            plan("beta", PlanStatus::Active, pid),
            plan("alpha", PlanStatus::Draft, pid),
            plan("aardvark", PlanStatus::Active, pid),
        ];
        sort_plans(&mut plans);
        let titles: Vec<&str> = plans.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["aardvark", "beta", "alpha", "zeta"]);

        // Re-sorting must not move anything.
        let before = plans.clone();
        sort_plans(&mut plans);
        assert_eq!(plans, before);
    }
}
