use crate::base::mount_base;
use crate::host_shell::HostShellSignal;
use leptos::prelude::*;
use leptos_router::{hooks::use_navigate, NavigateOptions};

fn current_path() -> String {
    web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_default()
}

#[component]
pub fn HostShellNav() -> impl IntoView {
    let config = use_context::<HostShellSignal>();
    let navigate = use_navigate();
    let base = mount_base();
    let graph = format!("{base}/graph");
    let activity = format!("{base}/activity");
    let agent_ops = format!("{base}/agent-ops");
    let time_machine = format!("{base}/time-machine");
    let tasks = format!("{base}/");
    let options = || NavigateOptions {
        resolve: false,
        ..Default::default()
    };

    let on_graph = {
        let navigate = navigate.clone();
        let graph = graph.clone();
        move |_| navigate(&graph, options())
    };
    let on_activity = {
        let navigate = navigate.clone();
        let activity = activity.clone();
        move |_| navigate(&activity, options())
    };
    let on_agent_ops = {
        let navigate = navigate.clone();
        let agent_ops = agent_ops.clone();
        move |_| navigate(&agent_ops, options())
    };
    let on_time_machine = {
        let navigate = navigate.clone();
        let time_machine = time_machine.clone();
        move |_| navigate(&time_machine, options())
    };
    let on_tasks = {
        let navigate = navigate.clone();
        move |_| navigate(&tasks, options())
    };

    let graph_path = graph.clone();
    let activity_path = activity.clone();
    let agent_ops_path = agent_ops.clone();
    let time_machine_path = time_machine.clone();
    let tasks_paths = (
        graph.clone(),
        activity.clone(),
        agent_ops.clone(),
        time_machine.clone(),
    );

    view! {
        <div class="host-shell-nav">
            // Graph / Tasks nav buttons — always visible regardless of host shell.
            <button
                class=move || if current_path().starts_with(&graph_path) {
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
                class=move || if current_path().starts_with(&activity_path) {
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
                class=move || if current_path().starts_with(&agent_ops_path) {
                    "host-shell-nav__link host-shell-nav__link--active"
                } else {
                    "host-shell-nav__link"
                }
                type="button"
                on:click=on_agent_ops
            >
                "Agent Ops"
            </button>
            <button
                class=move || if current_path().starts_with(&time_machine_path) {
                    "host-shell-nav__link host-shell-nav__link--active"
                } else {
                    "host-shell-nav__link"
                }
                type="button"
                on:click=on_time_machine
            >
                "Time Machine"
            </button>
            <button
                class=move || {
                    let path = current_path();
                    let (graph, activity, agent_ops, time_machine) = &tasks_paths;
                    if !path.starts_with(graph)
                        && !path.starts_with(activity)
                        && !path.starts_with(agent_ops)
                        && !path.starts_with(time_machine)
                    {
                        "host-shell-nav__link host-shell-nav__link--active"
                    } else {
                        "host-shell-nav__link"
                    }
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
                                    on:click=move |_| {
                                        if let Some(window) = web_sys::window() {
                                            let _ = window.location().assign(&href);
                                        }
                                    }
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
