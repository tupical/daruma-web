use crate::host_shell::HostShellSignal;
use leptos::prelude::*;

fn navigate_to(url: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().assign(url);
    }
}

#[component]
pub fn HostShellNav() -> impl IntoView {
    let config = use_context::<HostShellSignal>();

    view! {
        <Show when=move || config.is_some_and(|cfg| cfg.get().is_some())>
            {move || {
                let cfg = config.expect("checked by Show").get().expect("checked by Show");
                let label = cfg
                    .current_workspace_label
                    .clone()
                    .unwrap_or_else(|| "Workspace".to_string());
                let url = cfg.primary_url().map(str::to_string);

                view! {
                    <div class="host-shell-nav">
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
                    </div>
                }
            }}
        </Show>
    }
}
