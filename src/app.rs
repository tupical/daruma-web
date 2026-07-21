use crate::components::{
    ActivityFeed, AgentOpsPanel, ArtifactsPanel, DocumentsPanel, PlansPanel, ProjectSettingsPanel,
    Shell, TaskList, TimeMachine, WorkspaceGraph,
};
use crate::projects_ctx::{resolve_filter, ProjectsCtx};
use leptos::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::hooks::use_params_map;
use leptos_router::path;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceTab {
    Tasks,
    Plans,
}

#[component]
pub fn App() -> impl IntoView {
    let base = crate::base::mount_base();
    view! {
        <Router base=base>
            <Routes fallback=|| view! { <WorkspaceApp /> }>
                <Route path=path!("/graph") view=GraphApp />
                <Route path=path!("/activity") view=ActivityApp />
                <Route path=path!("/agent-ops") view=AgentOpsApp />
                <Route path=path!("/time-machine") view=TimeMachineApp />
                // `:project?` folds the 1- and 2-segment forms into a single
                // route match (same RouteMatchId), so switching between
                // `/{ws}` and `/{ws}/{proj}` only updates params instead of
                // tearing down and recreating WorkspaceApp's owner (the
                // nested router's rebuild diffs matches by id and only
                // replaces the subtree when the id changes).
                <Route path=path!("/:workspace/:project?") view=WorkspaceApp />
                <Route path=path!("/") view=WorkspaceApp />
                <Route path=path!("/app/:project?") view=WorkspaceApp />
            </Routes>
        </Router>
    }
}

#[component]
fn GraphApp() -> impl IntoView {
    view! {
        <Shell app_class="app app--graph" main_class="main main--graph">
            <WorkspaceGraph />
        </Shell>
    }
}

#[component]
fn ActivityApp() -> impl IntoView {
    view! {
        <Shell app_class="app app--activity" main_class="main main--activity">
            <ActivityFeed />
        </Shell>
    }
}

#[component]
fn AgentOpsApp() -> impl IntoView {
    view! {
        <Shell app_class="app app--agent-ops" main_class="main main--agent-ops" project_bar=true>
            <AgentOpsPanel />
        </Shell>
    }
}

#[component]
fn TimeMachineApp() -> impl IntoView {
    view! {
        <Shell app_class="app app--time-machine" main_class="main main--time-machine">
            <TimeMachine />
        </Shell>
    }
}

#[component]
fn WorkspaceApp() -> impl IntoView {
    let ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let params = use_params_map();
    let tab = RwSignal::new(WorkspaceTab::Tasks);

    Effect::new(move |_| {
        let map = params.get();
        if let Some(seg) = map.get("project").filter(|s| !s.is_empty()) {
            ctx.current_filter
                .set(resolve_filter(seg.as_str(), &ctx.projects.get()));
        } else if map.get("workspace").is_some() {
            ctx.current_filter
                .set(resolve_filter("all", &ctx.projects.get()));
        }
    });

    let aside = view! {
        <aside class="plans-aside">
            <DocumentsPanel />
            <ArtifactsPanel />
            <ProjectSettingsPanel />
        </aside>
    }
    .into_any();

    view! {
        <Shell app_class="app" main_class="main" project_bar=true aside=aside>
            <div class="project-bar workspace-view-tabs">
                <button
                    type="button"
                    class=move || if tab.get() == WorkspaceTab::Tasks { "tab active" } else { "tab" }
                    on:click=move |_| tab.set(WorkspaceTab::Tasks)
                >
                    "Задачи"
                </button>
                <button
                    type="button"
                    class=move || if tab.get() == WorkspaceTab::Plans { "tab active" } else { "tab" }
                    on:click=move |_| tab.set(WorkspaceTab::Plans)
                >
                    "Планы"
                </button>
            </div>
            {move || match tab.get() {
                WorkspaceTab::Tasks => view! { <TaskList /> }.into_any(),
                WorkspaceTab::Plans => view! { <PlansPanel /> }.into_any(),
            }}
        </Shell>
    }
}
