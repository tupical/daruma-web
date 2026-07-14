use crate::projects_ctx::{canonical_path, ProjectFilter, ProjectsCtx};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

#[component]
pub fn ProjectBar() -> impl IntoView {
    let ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let projects = ctx.projects;
    let current_filter = ctx.current_filter;
    let workspace_slug = ctx.workspace_slug;
    let projects_error = ctx.projects_error;
    let navigate = use_navigate();

    let select_filter = Callback::new(move |filter: ProjectFilter| {
        current_filter.set(filter.clone());
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
