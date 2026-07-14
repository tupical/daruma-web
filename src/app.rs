use crate::components::{
    ActivityFeed, DocumentsPanel, HostShellNav, PlansPanel, ProjectBar, TaskList, WorkspaceGraph,
};
use crate::projects_ctx::{resolve_filter, ProjectsCtx};
use leptos::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::hooks::use_params_map;
use leptos_router::path;

#[component]
pub fn App() -> impl IntoView {
    view! {
        <Router>
            <Routes fallback=|| view! { <WorkspaceApp /> }>
                <Route path=path!("/graph") view=GraphApp />
                <Route path=path!("/activity") view=ActivityApp />
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
        <div class="app app--graph">
            <div class="header">
                <div class="header-row">
                    <div class="viewer-title">"Daruma OSS Viewer"</div>
                    <HostShellNav />
                </div>
            </div>
            <main class="main main--graph">
                <WorkspaceGraph />
            </main>
        </div>
    }
}

#[component]
fn ActivityApp() -> impl IntoView {
    view! {
        <div class="app app--activity">
            <div class="header">
                <div class="header-row">
                    <div class="viewer-title">"Daruma OSS Viewer"</div>
                    <HostShellNav />
                </div>
            </div>
            <main class="main main--activity">
                <ActivityFeed />
            </main>
        </div>
    }
}

#[component]
fn WorkspaceApp() -> impl IntoView {
    let ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let params = use_params_map();

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

    view! {
        <div class="app">
            <div class="header">
                <div class="header-row">
                    <div class="viewer-title">"Daruma OSS Viewer"</div>
                    <HostShellNav />
                </div>
                <ProjectBar />
            </div>
            <main class="main">
                <TaskList />
            </main>
            <aside class="plans-aside">
                <PlansPanel />
                <DocumentsPanel />
            </aside>
        </div>
    }
}
