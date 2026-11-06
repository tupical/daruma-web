//! WebSocket v2 client for `/v1/ws`.
//!
//! Provides a reactive stream of [`EventEnvelope`] events via Leptos signals.
//! No UI: this module is consumed by frontend components.
//!
//! ## Connection lifecycle
//!
//! 1. `spawn_ws` starts the connect-loop and returns [`WsCtx`].
//! 2. On each connect: open WS → read Hello (server_seq) → **catch-up** via
//!    `GET /v1/events?since={resume_seq}&limit=500` (pages until caught up) →
//!    send Subscribe (all 11 channels, `since_seq` for gap-free delivery) →
//!    page Snapshots until the backlog is closed → stream live events. The
//!    order lives in [`crate::ws_session`]; this module is the browser side.
//! 3. On disconnect: exponential back-off, then reconnect from the end of the
//!    fully processed range.
//! 4. On server-initiated Resync: reconnect immediately with the given seq.
//!
//! All events are forwarded to both [`WsCtx`] (backward compat) and the
//! shared [`EventStoreCtx`] (new API for workers 2 & 3).

use daruma_api_dto::ws::{WsClientMessage, WsServerMessage};
use daruma_events::EventEnvelope;
use futures_util::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::api;
use crate::event_store::{ConnState, EventStoreCtx};
use crate::ws_session::{run_session, ResumeCursor, SessionPort, SessionResult};

const WS_PROTOCOL: &str = "daruma.v1";

// Build the WebSocket base URL from the current page origin: `wss://host` on
// https, `ws://host` on http. In dev (`trunk serve`) the Trunk proxy in
// Trunk.toml forwards /v1/ws to the local API.
fn ws_base() -> String {
    let location = web_sys::window().expect("window").location();
    let proto = match location.protocol().as_deref() {
        Ok("https:") => "wss:",
        _ => "ws:",
    };
    let host = location.host().unwrap_or_default();
    format!("{proto}//{host}")
}

// ── Public types ──────────────────────────────────────────────────────────────

/// Current connection state.
#[derive(Clone, Debug)]
pub enum WsStatus {
    Connecting,
    CatchingUp,
    Open,
    Reconnecting {
        in_secs: u32,
    },
    // `connect_loop` retries forever (Connecting <-> Reconnecting), so this
    // variant is never constructed today — kept for a future deliberate
    // "give up" path and matched defensively by status_bar.rs.
    #[allow(dead_code)]
    Closed,
}

/// Handle returned by [`spawn_ws`].  Cheaply cloneable — all fields are
/// Leptos `ReadSignal`s backed by `Arc`-shared state.
///
/// **Backward-compat note:** existing components consume `events` and `status`
/// directly from `WsCtx`.  New components (workers 2-3) should use
/// [`EventStoreCtx`] instead for richer slicing.
#[derive(Clone)]
pub struct WsCtx {
    pub events: ReadSignal<Vec<EventEnvelope>>,
    pub status: ReadSignal<WsStatus>,
    pub server_seq: ReadSignal<u64>,
}

// ── Backoff schedule ──────────────────────────────────────────────────────────

/// Seconds to wait before each reconnect attempt (index = attempt number).
/// Beyond the table length, the last value (30) is used.
const BACKOFF_SECS: &[u32] = &[1, 2, 4, 8, 16, 30];

fn backoff_secs(attempt: u32) -> u32 {
    let idx = (attempt as usize).min(BACKOFF_SECS.len() - 1);
    BACKOFF_SECS[idx]
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Spawn the WebSocket connect-loop and return reactive handles.
///
/// The returned [`WsCtx`] is intended to be placed in Leptos context via
/// `provide_context(ws_ctx)` so components can read signals reactively.
///
/// `store` receives every event in addition to `WsCtx.events` so that
/// workers 2-3 can consume slices without touching transport.
pub fn spawn_ws(token: String, store: EventStoreCtx) -> WsCtx {
    let (events_r, events_w) = RwSignal::new(Vec::<EventEnvelope>::new()).split();
    let (status_r, status_w) = RwSignal::new(WsStatus::Connecting).split();
    let (seq_r, seq_w) = RwSignal::new(0u64).split();

    spawn_local(connect_loop(token, events_w, status_w, seq_w, store));

    WsCtx {
        events: events_r,
        status: status_r,
        server_seq: seq_r,
    }
}

// ── Connect loop ──────────────────────────────────────────────────────────────

async fn connect_loop(
    token: String,
    events_w: WriteSignal<Vec<EventEnvelope>>,
    status_w: WriteSignal<WsStatus>,
    seq_w: WriteSignal<u64>,
    store: EventStoreCtx,
) {
    let mut attempt: u32 = 0;
    // Survives reconnects; starts at 0, i.e. fetch from the beginning.
    let mut cursor = ResumeCursor::default();

    loop {
        status_w.set(WsStatus::Connecting);
        store.set_conn_state(ConnState::Connecting);

        let url = if token.is_empty() {
            format!("{}/v1/ws", ws_base())
        } else {
            format!(
                "{}/v1/ws?token={}",
                ws_base(),
                crate::auth::encode_component(&token)
            )
        };
        let ws = if token.is_empty() {
            WebSocket::open_with_protocol(&url, WS_PROTOCOL)
        } else {
            let protocols = [WS_PROTOCOL.to_string(), format!("bearer.{token}")];
            WebSocket::open_with_protocols(&url, &protocols)
        };
        let ws = match ws {
            Ok(ws) => ws,
            Err(e) => {
                let wait = backoff_secs(attempt);
                leptos::logging::warn!("[ws] open failed: {e:?}, retrying in {wait}s");
                status_w.set(WsStatus::Reconnecting { in_secs: wait });
                store.set_conn_state(ConnState::Offline {
                    retry_in_secs: wait,
                });
                TimeoutFuture::new(wait * 1000).await;
                attempt += 1;
                continue;
            }
        };

        let mut port = BrowserPort {
            ws,
            events_w: &events_w,
            status_w: &status_w,
            seq_w: &seq_w,
            store: &store,
        };
        match run_session(&mut port, &mut cursor).await {
            SessionResult::Resync { from_seq } => {
                cursor.rewind(from_seq);
                attempt = 0; // server-initiated resync is not an error
            }
            SessionResult::Disconnected => {
                let wait = backoff_secs(attempt);
                leptos::logging::warn!("[ws] disconnected, retrying in {wait}s");
                status_w.set(WsStatus::Reconnecting { in_secs: wait });
                store.set_conn_state(ConnState::Offline {
                    retry_in_secs: wait,
                });
                TimeoutFuture::new(wait * 1000).await;
                attempt += 1;
            }
        }
    }
}

// ── Browser port ──────────────────────────────────────────────────────────────

/// One open socket plus the signals a session publishes to.
struct BrowserPort<'a> {
    ws: WebSocket,
    events_w: &'a WriteSignal<Vec<EventEnvelope>>,
    status_w: &'a WriteSignal<WsStatus>,
    seq_w: &'a WriteSignal<u64>,
    store: &'a EventStoreCtx,
}

impl SessionPort for BrowserPort<'_> {
    async fn next_frame(&mut self) -> Option<WsServerMessage> {
        while let Some(msg_result) = self.ws.next().await {
            let txt = match msg_result {
                Ok(Message::Text(t)) => t,
                Ok(Message::Bytes(_)) => continue, // ignore binary
                Err(e) => {
                    leptos::logging::warn!("[ws] receive error: {e:?}");
                    return None;
                }
            };
            match serde_json::from_str::<WsServerMessage>(&txt) {
                Ok(frame) => return Some(frame),
                Err(e) => leptos::logging::warn!("[ws] decode error: {e} — raw: {txt}"),
            }
        }
        None
    }

    async fn send(&mut self, frame: &WsClientMessage) -> bool {
        match serde_json::to_string(frame) {
            Ok(txt) => self.ws.send(Message::Text(txt)).await.is_ok(),
            Err(_) => false,
        }
    }

    async fn events_since(
        &mut self,
        since: u64,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, String> {
        api::events_since(since, limit)
            .await
            .map_err(|e| e.to_string())
    }

    fn hello(&mut self, server_seq: u64) {
        self.seq_w.set(server_seq);
    }

    fn deliver(&mut self, events: Vec<EventEnvelope>, covered_to: u64) {
        self.seq_w.update(|s| *s = (*s).max(covered_to));
        if !events.is_empty() {
            self.events_w.update(|v| v.extend(events.clone()));
            self.store.push_batch(events);
        }
    }

    fn set_conn_state(&mut self, state: ConnState) {
        match state {
            ConnState::CatchingUp => self.status_w.set(WsStatus::CatchingUp),
            ConnState::Live => self.status_w.set(WsStatus::Open),
            ConnState::Connecting | ConnState::Offline { .. } => {}
        }
        self.store.set_conn_state(state);
    }
}
