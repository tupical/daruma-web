use crate::components::{DocumentsPanel, HostShellNav, PlansPanel, ProjectBar, TaskList};
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
                <Route path=path!("/:workspace/:project") view=WorkspaceApp />
                <Route path=path!("/:workspace") view=WorkspaceApp />
                <Route path=path!("/") view=WorkspaceApp />
                <Route path=path!("/app") view=WorkspaceApp />
                <Route path=path!("/app/") view=WorkspaceApp />
                <Route path=path!("/app/:project") view=WorkspaceApp />
            </Routes>
        </Router>
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
                    <div class="viewer-title">"TaskAgent OSS Viewer"</div>
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
