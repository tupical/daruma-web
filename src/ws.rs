// Status / reconnect-counter fields are reserved for the future WS status indicator
// (follow-up UI); silencing dead_code until a component consumes them.
#![allow(dead_code)]
//! WebSocket v2 client for `/v1/ws`.
//!
//! Provides a reactive stream of [`EventEnvelope`] events via Leptos signals.
//! No UI — this module is consumed by W3 components.

use futures_util::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use taskagent_api_dto::ws::{WsClientMessage, WsServerMessage};
use taskagent_events::{Channel, EventEnvelope};
use wasm_bindgen_futures::spawn_local;

const WS_PROTOCOL: &str = "taskagent.v1";

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
    Open,
    Reconnecting { in_secs: u32 },
    Closed,
}

/// Handle returned by [`spawn_ws`].  Cheaply cloneable — all fields are
/// Leptos `ReadSignal`s backed by `Arc`-shared state.
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
/// `provide_context(ws_ctx)` so W3 components can read signals reactively.
pub fn spawn_ws(token: String) -> WsCtx {
    let (events_r, events_w) = RwSignal::new(Vec::<EventEnvelope>::new()).split();
    let (status_r, status_w) = RwSignal::new(WsStatus::Connecting).split();
    let (seq_r, seq_w) = RwSignal::new(0u64).split();

    spawn_local(connect_loop(token, events_w, status_w, seq_w));

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
) {
    let mut attempt: u32 = 0;
    let mut since_seq: Option<u64> = None;

    loop {
        status_w.set(WsStatus::Connecting);

        let url = format!("{}/v1/ws", ws_base());
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
                TimeoutFuture::new(wait * 1000).await;
                attempt += 1;
                continue;
            }
        };

        // Run the session; returns `since_seq` to use for the next connect.
        match run_session(ws, since_seq, &events_w, &status_w, &seq_w).await {
            SessionResult::Resync { from_seq } => {
                since_seq = Some(from_seq);
                attempt = 0; // server-initiated resync is not an error
            }
            SessionResult::Disconnected => {
                let wait = backoff_secs(attempt);
                leptos::logging::warn!("[ws] disconnected, retrying in {wait}s");
                status_w.set(WsStatus::Reconnecting { in_secs: wait });
                TimeoutFuture::new(wait * 1000).await;
                attempt += 1;
            }
        }
    }
}

enum SessionResult {
    /// Server sent Resync — reconnect immediately with `from_seq`.
    Resync { from_seq: u64 },
    /// Connection dropped unexpectedly.
    Disconnected,
}

async fn run_session(
    mut ws: WebSocket,
    since_seq: Option<u64>,
    events_w: &WriteSignal<Vec<EventEnvelope>>,
    status_w: &WriteSignal<WsStatus>,
    seq_w: &WriteSignal<u64>,
) -> SessionResult {
    // 1. Read Hello frame.
    let hello = match ws.next().await {
        Some(Ok(Message::Text(txt))) => match serde_json::from_str::<WsServerMessage>(&txt) {
            Ok(msg) => msg,
            Err(e) => {
                leptos::logging::warn!("[ws] bad Hello frame: {e}");
                return SessionResult::Disconnected;
            }
        },
        other => {
            leptos::logging::warn!("[ws] expected Hello, got {other:?}");
            return SessionResult::Disconnected;
        }
    };

    let server_seq = match hello {
        WsServerMessage::Hello { server_seq, .. } => {
            seq_w.set(server_seq);
            server_seq
        }
        _ => {
            leptos::logging::warn!("[ws] first frame was not Hello");
            return SessionResult::Disconnected;
        }
    };

    status_w.set(WsStatus::Open);

    // 2. Send Subscribe.
    //
    // The server defaults `channels: None` to `[Channel::Tasks]` only, which
    // drops plan-lifecycle events.  List the channels the UI actually consumes
    // so events like `PlanStatusChanged` reach `WsCtx.events` and trigger the
    // plan-panel resource refetch in real time.
    let sub = WsClientMessage::Subscribe {
        since_seq,
        projects: None,
        channels: Some(vec![Channel::Tasks, Channel::Plans, Channel::Documents]),
        assignee: None,
        verb: None,
        parent_plan: None,
    };
    if let Ok(txt) = serde_json::to_string(&sub) {
        if ws.send(Message::Text(txt)).await.is_err() {
            leptos::logging::warn!("[ws] failed to send Subscribe");
            return SessionResult::Disconnected;
        }
    }

    let _ = server_seq; // used above

    // 3. Message loop.
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
                events_w.update(|v| v.push(envelope));
                seq_w.update(|s| *s = (*s).max(new_seq));
            }
            WsServerMessage::Snapshot {
                events, next_seq, ..
            } => {
                let max_seq = events.iter().map(|e| e.seq).max().unwrap_or(0);
                events_w.update(|v| v.extend(events));
                if let Some(ns) = next_seq {
                    seq_w.update(|s| *s = (*s).max(ns));
                } else {
                    seq_w.update(|s| *s = (*s).max(max_seq));
                }
            }
            WsServerMessage::Resync { from_seq, dropped } => {
                leptos::logging::warn!("[ws] Resync from_seq={from_seq} dropped={dropped}");
                return SessionResult::Resync { from_seq };
            }
            WsServerMessage::Ping => {
                // Reply with Pong.
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

    SessionResult::Disconnected
}
