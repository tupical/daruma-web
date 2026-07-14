//! Compact, read-only status bar — WS connection state + API healthz.
//!
//! No write controls: this is pure observability, meant to sit unobtrusively
//! in the shell footer. Two independent poll triggers keep it fresh without
//! hammering the server: a periodic timer ([`POLL_SECS`]) and an immediate
//! poll whenever the tab regains focus.

use crate::api;
use crate::ws::{WsCtx, WsStatus};
use gloo_timers::future::TimeoutFuture;
use leptos::ev;
use leptos::leptos_dom::helpers::window_event_listener;
use leptos::prelude::*;
use leptos::reactive::owner::Owner;

/// How often to re-poll `/v1/healthz` (and, best-effort, workspacegraph
/// status) in the background. Kept moderate — this is a passive footer, not
/// a live dashboard.
const POLL_SECS: u32 = 30;

fn ws_status_view(status: &WsStatus) -> (&'static str, String) {
    match status {
        WsStatus::Connecting => ("connecting", "connecting".to_string()),
        WsStatus::CatchingUp => ("catching-up", "catching up".to_string()),
        WsStatus::Open => ("open", "live".to_string()),
        WsStatus::Reconnecting { in_secs } => {
            ("reconnecting", format!("reconnecting in {in_secs}s"))
        }
        WsStatus::Closed => ("closed", "offline".to_string()),
    }
}

/// One fetch pass: healthz always, workspacegraph status best-effort (the
/// same endpoint the graph view already relies on, so a viewer token that
/// can render `/graph` can read this too). Either failing just leaves the
/// corresponding signal at its previous value — no error UI, this is
/// opportunistic.
async fn poll_once(
    healthz: RwSignal<Option<api::HealthzInfo>>,
    graph_lag: RwSignal<Option<i64>>,
    server_seq: ReadSignal<u64>,
) {
    if let Ok(h) = api::healthz().await {
        healthz.set(Some(h));
    }
    if let Ok(status) = api::workspacegraph_status().await {
        if let Some(last) = status.last_event_seq {
            let seq = server_seq.get_untracked();
            graph_lag.set((seq >= last).then_some((seq - last) as i64));
        }
    }
}

#[component]
pub fn StatusBar() -> impl IntoView {
    let ws_ctx = use_context::<WsCtx>().expect("WsCtx");
    let ws_status = ws_ctx.status;
    let server_seq = ws_ctx.server_seq;

    let healthz: RwSignal<Option<api::HealthzInfo>> = RwSignal::new(None);
    let graph_lag: RwSignal<Option<i64>> = RwSignal::new(None);

    // Periodic poll loop. Cancel-on-cleanup like the route-scoped fetches in
    // task_list.rs / plans_panel.rs: StatusBar is mounted inside the
    // route-level shells (GraphApp/ActivityApp/WorkspaceApp) and gets torn
    // down when navigating between them, so a plain spawn would resume and
    // touch disposed signals after unmount.
    leptos::task::spawn_local_scoped_with_cancellation(async move {
        loop {
            poll_once(healthz, graph_lag, server_seq).await;
            TimeoutFuture::new(POLL_SECS * 1000).await;
        }
    });

    // Extra poll when the tab regains focus, so a long-backgrounded tab
    // doesn't show stale status until the next timer tick. The listener
    // callback runs outside the reactive ownership tree (per
    // `window_event_listener`'s own contract), so the owner captured here is
    // re-established inside the callback before spawning — otherwise the
    // scoped spawn's `on_cleanup` would have no owner to attach to.
    let owner = Owner::current().expect("StatusBar runs inside a reactive owner");
    let listener = window_event_listener(ev::focus, move |_| {
        owner.with(|| {
            leptos::task::spawn_local_scoped_with_cancellation(poll_once(
                healthz, graph_lag, server_seq,
            ));
        });
    });
    on_cleanup(move || listener.remove());

    view! {
        <footer class="status-bar">
            {move || {
                let (modifier, label) = ws_status_view(&ws_status.get());
                let dot_class = format!("status-bar__dot status-bar__dot--{modifier}");
                view! {
                    <span class=dot_class></span>
                    <span class="status-bar__ws">{label}</span>
                }
            }}
            <span class="status-bar__sep">"·"</span>
            <span class="status-bar__api">
                {move || match healthz.get() {
                    Some(h) => format!("api {} · core {}", h.api_version, h.core_version),
                    None => "api …".to_string(),
                }}
            </span>
            {move || {
                graph_lag.get().map(|lag| view! {
                    <span class="status-bar__sep">"·"</span>
                    <span class="status-bar__lag">{format!("graph lag {lag}")}</span>
                })
            }}
        </footer>
    }
}
