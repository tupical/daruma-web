use crate::projects_ctx::{canonical_path, ProjectFilter, ProjectsCtx};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

/// Routes that render their own view and carry the project bar only to scope
/// it. Picking a project on one of these must refilter in place, not navigate.
const SECTION_ROUTES: [&str; 4] = ["/graph", "/activity", "/agent-ops", "/time-machine"];

fn is_section_route(route_path: &str) -> bool {
    SECTION_ROUTES
        .iter()
        .any(|section| route_path == *section || route_path.starts_with(&format!("{section}/")))
}

fn on_section_route() -> bool {
    is_section_route(&crate::base::route_path())
}

#[component]
pub fn ProjectBar() -> impl IntoView {
    let ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let projects = ctx.projects;
    let current_filter = ctx.current_filter;
    let workspace_slug = ctx.workspace_slug;
    let projects_error = ctx.projects_error;
    let navigate = use_navigate();

    // On a section route the bar is a pure filter: set the signal and stay put,
    // otherwise picking a project on /graph would throw you back to the task
    // list. On the workspace route it navigates instead and `WorkspaceApp`'s
    // route effect is the single writer of `current_filter` — setting it here
    // too pushed an update into a subtree the very next `navigate` was about to
    // dispose (`/` and `/app/:project?` are different route matches), and the
    // panels read the disposed memos mid-flush: "you tried to access a reactive
    // value … already disposed". No navigation here, so no teardown, so safe.
    let select_filter = Callback::new(move |filter: ProjectFilter| {
        if on_section_route() {
            current_filter.set(filter);
            return;
        }
        let path = canonical_path(&workspace_slug.get(), &filter, &projects.get());
        navigate(&path, Default::default());
    });

    view! {
        <Show when=move || projects_error.get().is_some() fallback=|| view! { <></> }>
            <p class="fetch-error__message">
                { move || projects_error.get().unwrap_or_default() }
            </p>
        </Show>
        <div class="project-bar">
            <button
                type="button"
                class=move || {
                    if current_filter.get() == ProjectFilter::All {
                        "tab active"
                    } else {
                        "tab"
                    }
                }
                title="Show every task"
                on:click=move |_| select_filter.run(ProjectFilter::All)
            >
                "All"
            </button>
            <button
                type="button"
                class=move || {
                    if current_filter.get() == ProjectFilter::Inbox {
                        "tab active"
                    } else {
                        "tab"
                    }
                }
                title="Tasks not assigned to any project"
                on:click=move |_| select_filter.run(ProjectFilter::Inbox)
            >
                "Inbox"
            </button>
            <For
                each=move || projects.get()
                key=|p| p.id
                let:project
            >
                {
                    let pid = project.id;
                    let title = project.title.clone();
                    let select_filter = select_filter;
                    view! {
                        <button
                            type="button"
                            class=move || {
                                if current_filter.get() == ProjectFilter::Of(pid) {
                                    "tab active"
                                } else {
                                    "tab"
                                }
                            }
                            on:click=move |_| select_filter.run(ProjectFilter::Of(pid))
                        >
                            {title}
                        </button>
                    }
                }
            </For>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::is_section_route;

    #[test]
    fn section_routes_filter_in_place_workspace_routes_navigate() {
        assert!(is_section_route("/graph"));
        assert!(is_section_route("/activity"));
        assert!(is_section_route("/time-machine"));
        assert!(is_section_route("/graph/"));

        assert!(!is_section_route("/"));
        assert!(!is_section_route("/app/all"));
        assert!(!is_section_route("/acme/daruma-web"));
        // Prefix match must not swallow a workspace whose slug starts the same.
        assert!(!is_section_route("/graphite/all"));
    }
}
