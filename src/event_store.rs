//! Unified reactive event store shared across the frontend.
//!
//! # Architecture
//!
//! `EventStoreCtx` is provided once at the root (in `main.rs`) via
//! `provide_context(EventStoreCtx::new())`.  The WebSocket layer (ws.rs)
//! pushes [`EventEnvelope`] items into it via [`EventStoreCtx::push`] and
//! [`EventStoreCtx::push_batch`].
//!
//! # Public signals
//!
//! | Signal | Type | Description |
//! |---|---|---|
//! | `all_events` | `ReadSignal<Vec<EventEnvelope>>` | Chronological append-only log of every envelope received. Consumed by the activity feed (worker-2). |
//! | `graph_events` | `ReadSignal<Vec<EventEnvelope>>` | Subset filtered to graph-relevant channels (Tasks, Plans, Runs, Documents). Consumed by WorkspaceGraph (worker-3). |
//! | `conn_state` | `ReadSignal<ConnState>` | Connection lifecycle state. |
//! | `server_seq` | `ReadSignal<u64>` | Highest seq seen so far. |
//!
//! # Consuming a slice
//!
//! ```rust,ignore
//! let store = use_context::<EventStoreCtx>().expect("EventStoreCtx");
//!
//! // Subscribe to all events (feed):
//! let events = store.all_events;
//!
//! // Subscribe to graph-relevant events only:
//! let graph = store.graph_events;
//!
//! // React to connection state:
//! let conn = store.conn_state;
//! ```
//!
//! Both slices are `ReadSignal<Vec<EventEnvelope>>` — components call `.get()`
//! inside a reactive closure or `Effect` to re-run when new events arrive.
//!
//! # Thread-safety note
//!
//! All signals are created on the main WASM thread and must only be written
//! from `spawn_local` futures on the same thread.  `EventStoreCtx` is `Clone`
//! (all fields are `Copy` signals) so it can be captured by multiple closures.

use leptos::prelude::*;
use taskagent_events::{Channel, EventEnvelope};

/// Connection lifecycle state exposed to UI components.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnState {
    /// Initial connect in progress (no session established yet).
    Connecting,
    /// Session open; fetching missed events since last known seq.
    CatchingUp,
    /// Live: WS session open, catch-up complete, all events are real-time.
    Live,
    /// Connection lost; will retry after back-off.
    Offline { retry_in_secs: u32 },
}

/// Channels whose events are relevant to the workspace graph view.
///
/// Tasks, Plans, Runs, Documents, and Artifacts form nodes/edges in the graph.
/// Comments, AgentStatus, Presence, Webhooks, AiOps, WorkUnits are
/// activity-feed events and are filtered out of the graph slice.
const GRAPH_CHANNELS: &[Channel] = &[
    Channel::Tasks,
    Channel::Plans,
    Channel::Runs,
    Channel::Documents,
    Channel::Artifacts,
];

fn is_graph_relevant(env: &EventEnvelope) -> bool {
    GRAPH_CHANNELS.contains(&env.payload.channel())
}

/// Reactive event store context.  Place in Leptos context via
/// `provide_context(EventStoreCtx::new())`.
#[derive(Clone, Copy)]
pub struct EventStoreCtx {
    /// All events in arrival order (append-only).  Used by the activity feed.
    pub all_events: ReadSignal<Vec<EventEnvelope>>,
    /// Graph-relevant events only (Tasks / Plans / Runs / Documents).
    pub graph_events: ReadSignal<Vec<EventEnvelope>>,
    /// Connection lifecycle state.
    pub conn_state: ReadSignal<ConnState>,
    /// Highest event seq seen so far (updated on every push).
    pub server_seq: ReadSignal<u64>,

    // Write handles — kept private; only ws.rs may call them via the methods below.
    all_w: WriteSignal<Vec<EventEnvelope>>,
    graph_w: WriteSignal<Vec<EventEnvelope>>,
    conn_w: WriteSignal<ConnState>,
    // Kept as RwSignal so we can call get_untracked() without needing a ReadSignal.
    seq_rw: RwSignal<u64>,
}

impl EventStoreCtx {
    /// Create a new store.  Call once at the application root.
    pub fn new() -> Self {
        let (all_r, all_w) = RwSignal::new(Vec::<EventEnvelope>::new()).split();
        let (graph_r, graph_w) = RwSignal::new(Vec::<EventEnvelope>::new()).split();
        let (conn_r, conn_w) = RwSignal::new(ConnState::Connecting).split();
        let seq_rw = RwSignal::new(0u64);

        Self {
            all_events: all_r,
            graph_events: graph_r,
            conn_state: conn_r,
            server_seq: seq_rw.read_only(),
            all_w,
            graph_w,
            conn_w,
            seq_rw,
        }
    }

    /// Append a single envelope.  Updates seq and both slices atomically
    /// (each signal write is independent but happens in the same microtask).
    pub fn push(&self, env: EventEnvelope) {
        let new_seq = env.seq;
        let graph = is_graph_relevant(&env);
        self.all_w.update(|v| v.push(env.clone()));
        if graph {
            self.graph_w.update(|v| v.push(env));
        }
        self.seq_rw.update(|s| *s = (*s).max(new_seq));
    }

    /// Append a batch of envelopes (e.g. a Snapshot or catch-up page).
    /// More efficient than calling `push` in a loop because it batches the
    /// `all_events` write into a single signal update.
    pub fn push_batch(&self, batch: Vec<EventEnvelope>) {
        if batch.is_empty() {
            return;
        }
        let max_seq = batch.iter().map(|e| e.seq).max().unwrap_or(0);
        let graph_batch: Vec<EventEnvelope> = batch
            .iter()
            .filter(|e| is_graph_relevant(e))
            .cloned()
            .collect();
        self.all_w.update(|v| v.extend(batch));
        if !graph_batch.is_empty() {
            self.graph_w.update(|v| v.extend(graph_batch));
        }
        self.seq_rw.update(|s| *s = (*s).max(max_seq));
    }

    /// Update the connection state signal.
    pub fn set_conn_state(&self, state: ConnState) {
        self.conn_w.set(state);
    }

    /// Current highest seq (non-reactive read; use `server_seq` signal in
    /// reactive contexts).
    pub fn current_seq(&self) -> u64 {
        self.seq_rw.get_untracked()
    }
}
