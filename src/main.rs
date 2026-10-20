use leptos::prelude::*;

mod api;
mod app;
mod auth;
mod base;
mod components;
mod event_store;
mod host_shell;
mod projects_ctx;
mod relations_ctx;
mod ws;

use app::App;

fn main() {
    console_error_panic_hook::set_once();
    wasm_bindgen_futures::spawn_local(async {
        if let Some(code) = auth::code_from_url() {
            if let Err(err) = auth::exchange_code(code).await {
                leptos::logging::error!("[auth] oauth exchange failed: {err}");
                mount_sign_in(Some(format!("OAuth sign-in failed: {err}")));
                return;
            }
            auth::clean_oauth_url();
        }

        let _ = auth::bootstrap();
        if auth::current().is_none() {
            // OAuth endpoints come from the server this bundle is served
            // next to; nothing redirects before they are known.
            let problem = match auth::discover_oauth().await {
                Ok(Some(endpoints)) => match auth::start_oauth(&endpoints).await {
                    Ok(()) => return,
                    Err(err) => Some(format!("OAuth sign-in failed: {err}")),
                },
                Ok(None) => None,
                Err(err) => Some(format!("OAuth discovery failed: {err}")),
            };
            if let Some(problem) = &problem {
                leptos::logging::error!("[auth] {problem}");
            }
            mount_sign_in(problem);
            return;
        }

        mount_app();
    });
}

fn mount_app() {
    let _ = auth::bootstrap();
    let store = event_store::EventStoreCtx::new();
    let ws_ctx = ws::spawn_ws(auth::current().unwrap_or_default(), store);
    leptos::mount::mount_to_body(move || {
        provide_context(store);
        provide_context(ws_ctx.clone());
        provide_context(host_shell::init_host_shell());
        provide_context(projects_ctx::init_projects_ctx());
        provide_context(relations_ctx::RelationsCtx::new());
        view! { <App /> }
    });
}

/// No token and no OAuth sign-in to start: say how to get in rather than
/// leave a blank page.
fn mount_sign_in(problem: Option<String>) {
    leptos::mount::mount_to_body(move || {
        view! {
            <main class="sign-in">
                <h1>"Daruma"</h1>
                {problem
                    .clone()
                    .map(|problem| view! { <p class="sign-in-error">{problem}</p> })}
                <p>
                    {if problem.is_some() {
                        "To sign in with an API token instead, open this page with "
                    } else {
                        "This server does not offer OAuth sign-in. Open this page with "
                    }}
                    <code>"?token=<API token>"</code>
                    "; the token is then kept in this browser's local storage."
                </p>
            </main>
        }
    });
}
