use leptos::prelude::*;

mod api;
mod app;
mod auth;
mod components;
mod projects_ctx;
mod relations_ctx;
mod ws;

use app::App;

fn main() {
    console_error_panic_hook::set_once();
    let _ = auth::bootstrap();
    let ws_ctx = ws::spawn_ws(auth::current().unwrap_or_default());
    leptos::mount::mount_to_body(move || {
        provide_context(ws_ctx.clone());
        provide_context(projects_ctx::init_projects_ctx());
        provide_context(relations_ctx::RelationsCtx::new());
        view! { <App /> }
    });
}
