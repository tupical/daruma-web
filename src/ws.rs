//! WebSocket v2 client for `/v1/ws`.
//!
//! Provides a reactive stream of [`EventEnvelope`] events via Leptos signals.
//! No UI: this module is consumed by frontend components.
//!
//! ## Connection lifecycle
//!
//! 1. `spawn_ws` starts the connect-loop and returns [`WsCtx`].
//! 2. On each connect: open WS → read Hello (server_seq) → **catch-up** via
//!    `GET /v1/events?since={last_seq}&limit=500` (pages until caught up) →
//!    send Subscribe (all 11 channels, `since_seq` for gap-free delivery) →
//!    stream live events.
//! 3. On disconnect: exponential back-off, then reconnect from last seq.
//! 4. On server-initiated Resync: reconnect immediately with the given seq.
//!
//! All events are forwarded to both [`WsCtx`] (backward compat) and the
//! shared [`EventStoreCtx`] (new API for workers 2 & 3).

use daruma_api_dto::ws::{WsClientMessage, WsServerMessage};
use daruma_events::{Channel, EventEnvelope};
use futures_util::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::api;
use crate::event_store::{ConnState, EventStoreCtx};

const WS_PROTOCOL: &str = "daruma.v1";

/// Maximum number of events fetched per catch-up page.
const CATCHUP_PAGE_SIZE: usize = 500;

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
    // `since_seq` tracks the highest seq successfully consumed across sessions.
    // On the first connect it is None (→ 0, i.e. fetch from the beginning).
    let mut since_seq: Option<u64> = None;

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

        match run_session(ws, since_seq, &events_w, &status_w, &seq_w, &store).await {
            SessionResult::Resync { from_seq } => {
                since_seq = Some(from_seq);
                attempt = 0; // server-initiated resync is not an error
            }
            SessionResult::Disconnected { last_seq } => {
                // Remember the highest seq so the next connect can catch up.
                if let Some(s) = last_seq {
                    since_seq = Some(s);
                }
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

enum SessionResult {
    /// Server sent Resync — reconnect immediately with `from_seq`.
    Resync { from_seq: u64 },
    /// Connection dropped unexpectedly; carries the highest seq seen so far.
    Disconnected { last_seq: Option<u64> },
}

async fn run_session(
    mut ws: WebSocket,
    since_seq: Option<u64>,
    events_w: &WriteSignal<Vec<EventEnvelope>>,
    status_w: &WriteSignal<WsStatus>,
    seq_w: &WriteSignal<u64>,
    store: &EventStoreCtx,
) -> SessionResult {
    // 1. Read Hello frame.
    let hello = match ws.next().await {
        Some(Ok(Message::Text(txt))) => match serde_json::from_str::<WsServerMessage>(&txt) {
            Ok(msg) => msg,
            Err(e) => {
                leptos::logging::warn!("[ws] bad Hello frame: {e}");
                return SessionResult::Disconnected { last_seq: None };
            }
        },
        other => {
            leptos::logging::warn!("[ws] expected Hello, got {other:?}");
            return SessionResult::Disconnected { last_seq: None };
        }
    };

    match hello {
        WsServerMessage::Hello { server_seq, .. } => {
            seq_w.set(server_seq);
        }
        _ => {
            leptos::logging::warn!("[ws] first frame was not Hello");
            return SessionResult::Disconnected { last_seq: None };
        }
    }

    // 2. Catch-up: fetch any events missed since the last known seq via HTTP.
    //    This fills the gap between the previous session and now, before we
    //    start streaming.  `since_seq` is the last seq from the prior session
    //    (or 0 on the very first connect).  We page until the server returns
    //    fewer events than the page size (meaning we've reached the head).
    let catchup_from = since_seq.unwrap_or(0);
    status_w.set(WsStatus::CatchingUp);
    store.set_conn_state(ConnState::CatchingUp);

    let mut cursor = catchup_from;
    let mut catchup = Vec::new();
    loop {
        match api::events_since(cursor, CATCHUP_PAGE_SIZE).await {
            Ok(page) => {
                let done = page.len() < CATCHUP_PAGE_SIZE;
                if let Some(last) = page.last() {
                    cursor = last.seq;
                }
                catchup.extend(page);
                if done {
                    break;
                }
            }
            Err(e) => {
                leptos::logging::warn!("[ws] catch-up failed: {e} — continuing without history");
                break;
            }
        }
    }
    if !catchup.is_empty() {
        let max_seq = catchup.iter().map(|e| e.seq).max().unwrap_or(0);
        events_w.update(|v| v.extend(catchup.clone()));
        seq_w.update(|s| *s = (*s).max(max_seq));
        store.push_batch(catchup);
    }

    // 3. Send Subscribe to all 11 channels.
    //
    // `since_seq` = cursor so the server delivers a Snapshot covering any
    // events that arrived between our last HTTP page and now, guaranteeing
    // gap-free delivery when transitioning from HTTP catch-up to live stream.
    let sub = WsClientMessage::Subscribe {
        since_seq: Some(cursor),
        projects: None,
        channels: Some(vec![
            Channel::Tasks,
            Channel::Comments,
            Channel::AgentStatus,
            Channel::Presence,
            Channel::Webhooks,
            Channel::Plans,
            Channel::Runs,
            Channel::Documents,
            Channel::AiOps,
            Channel::WorkUnits,
            Channel::Artifacts,
        ]),
        assignee: None,
        verb: None,
        parent_plan: None,
    };
    if let Ok(txt) = serde_json::to_string(&sub) {
        if ws.send(Message::Text(txt)).await.is_err() {
            leptos::logging::warn!("[ws] failed to send Subscribe");
            return SessionResult::Disconnected {
                last_seq: Some(cursor),
            };
        }
    }

    status_w.set(WsStatus::Open);
    store.set_conn_state(ConnState::Live);

    // 4. Message loop.
    while let Some(msg_result) = ws.next().await {
        let txt = match msg_result {
            Ok(Message::Text(t)) => t,
            Ok(Message::Bytes(_)) => continue, // ignore binary
            Err(e) => {
                leptos::logging::warn!("[ws] receive error: {e:?}");
                break;
            }
        };

        let server_msg = match serde_json::from_str::<WsServerMessage>(&txt) {
            Ok(m) => m,
            Err(e) => {
                leptos::logging::warn!("[ws] decode error: {e} — raw: {txt}");
                continue;
            }
        };

        match server_msg {
            WsServerMessage::Event { envelope } => {
                let new_seq = envelope.seq;
                events_w.update(|v| v.push(envelope.clone()));
                seq_w.update(|s| *s = (*s).max(new_seq));
                store.push(envelope);
            }
            WsServerMessage::Snapshot {
                events, next_seq, ..
            } => {
                let max_seq = events.iter().map(|e| e.seq).max().unwrap_or(0);
                if let Some(ns) = next_seq {
                    seq_w.update(|s| *s = (*s).max(ns));
                } else {
                    seq_w.update(|s| *s = (*s).max(max_seq));
                }
                events_w.update(|v| v.extend(events.clone()));
                store.push_batch(events);
            }
            WsServerMessage::Resync { from_seq, dropped } => {
                leptos::logging::warn!("[ws] Resync from_seq={from_seq} dropped={dropped}");
                return SessionResult::Resync { from_seq };
            }
            WsServerMessage::Ping => {
                if let Ok(txt) = serde_json::to_string(&WsClientMessage::Pong) {
                    let _ = ws.send(Message::Text(txt)).await;
                }
            }
            WsServerMessage::Hello { .. }
            | WsServerMessage::Ack { .. }
            | WsServerMessage::Error { .. }
            | WsServerMessage::Pong => {
                // Ignore
            }
        }
    }

    SessionResult::Disconnected {
        last_seq: Some(store.current_seq()),
    }
}
