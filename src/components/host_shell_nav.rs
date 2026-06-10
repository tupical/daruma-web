use crate::host_shell::HostShellSignal;
use leptos::prelude::*;

fn navigate_to(url: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().assign(url);
    }
}

fn current_path() -> String {
    web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_default()
}

#[component]
pub fn HostShellNav() -> impl IntoView {
    let config = use_context::<HostShellSignal>();

    let on_graph = move |_| navigate_to("/graph");
    let on_activity = move |_| navigate_to("/activity");
    let on_tasks = move |_| navigate_to("/");

    let is_graph = move || current_path().starts_with("/graph");
    let is_activity = move || current_path().starts_with("/activity");

    view! {
        <div class="host-shell-nav">
            // Graph / Tasks nav buttons — always visible regardless of host shell.
            <button
                class=move || if is_graph() {
                    "host-shell-nav__link host-shell-nav__link--active"
                } else {
                    "host-shell-nav__link"
                }
                type="button"
                on:click=on_graph
            >
                "Graph"
            </button>
            <button
                class=move || if is_activity() {
                    "host-shell-nav__link host-shell-nav__link--active"
                } else {
                    "host-shell-nav__link"
                }
                type="button"
                on:click=on_activity
            >
                "Activity"
            </button>
            <button
                class=move || if !is_graph() && !is_activity() {
                    "host-shell-nav__link host-shell-nav__link--active"
                } else {
                    "host-shell-nav__link"
                }
                type="button"
                on:click=on_tasks
            >
                "Tasks"
            </button>

            // Host-shell workspace label + link (only when shell context present).
            <Show when=move || config.is_some_and(|cfg| cfg.get().is_some())>
                {move || {
                    let cfg = config.expect("checked by Show").get().expect("checked by Show");
                    let label = cfg
                        .current_workspace_label
                        .clone()
                        .unwrap_or_else(|| "Workspace".to_string());
                    let url = cfg.primary_url().map(str::to_string);

                    view! {
                        <>
                            <span class="host-shell-nav__label">{label}</span>
                            {url.map(|href| view! {
                                <button
                                    class="host-shell-nav__link"
                                    type="button"
                                    on:click=move |_| navigate_to(&href)
                                >
                                    "Workspaces"
                                </button>
                            })}
                        </>
                    }
                }}
            </Show>
        </div>
    }
}
