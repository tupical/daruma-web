//! Protocol side of one `/v1/ws` session, free of browser types.
//!
//! [`run_session`] owns the order of a session: Hello, HTTP catch-up,
//! Subscribe, Snapshot pages, live stream. [`crate::ws`] plugs the browser
//! socket and the reactive signals in through [`SessionPort`]; the tests plug
//! a scripted server in.
//!
//! ## Closing the backlog
//!
//! The server sends at most one page per Subscribe and marks a partial one
//! with `has_more` / `next_seq`, then starts streaming everything past its
//! boundary. The page limit applies before the project and channel filters,
//! so a page may be empty and seq numbers may have holes: the range a page
//! covers is `(since_seq, next_seq]`, not the events it happens to carry.
//!
//! The session therefore stays in catch-up until a page arrives with
//! `has_more = false`. Each next page is requested by sending Subscribe again
//! from `next_seq` on the same socket, so it keeps working while HTTP is down.
//! Live events received meanwhile are held back: they sit past a range that
//! is not processed yet, and reporting them would move the cursor over it. A
//! later page can repeat a held event because every Subscribe moves the
//! boundary; [`ResumeCursor`] delivers such an event once.

use std::collections::HashSet;

use daruma_api_dto::ws::{WsClientMessage, WsServerMessage};
use daruma_events::{Channel, EventEnvelope};

use crate::event_store::ConnState;

/// Maximum number of events fetched per catch-up page.
const CATCHUP_PAGE_SIZE: usize = 500;

/// How far the history has been processed, kept across sessions.
///
/// `resume_seq` is the end of the fully processed range, the only safe place
/// to reconnect from. It never moves over a range still in flight, unlike the
/// highest seq ever received.
#[derive(Default)]
pub struct ResumeCursor {
    resume_seq: u64,
    delivered: HashSet<u64>,
}

impl ResumeCursor {
    pub fn resume_seq(&self) -> u64 {
        self.resume_seq
    }

    /// Mark everything up to `seq` as processed.
    fn advance(&mut self, seq: u64) {
        self.resume_seq = self.resume_seq.max(seq);
    }

    /// Advance over `batch` and return the events not delivered before.
    fn accept(&mut self, batch: Vec<EventEnvelope>) -> Vec<EventEnvelope> {
        batch
            .into_iter()
            .filter(|env| {
                self.advance(env.seq);
                self.delivered.insert(env.seq)
            })
            .collect()
    }

    /// Replay from `from_seq` on the next connect. A value past the processed
    /// range is ignored: the range between them was never delivered.
    pub fn rewind(&mut self, from_seq: u64) {
        self.resume_seq = self.resume_seq.min(from_seq);
    }
}

/// How a session ended.
pub enum SessionResult {
    /// Server sent Resync: reconnect at once, replaying from `from_seq`.
    Resync { from_seq: u64 },
    /// Connection dropped or the server broke the protocol: back off, then
    /// reconnect from [`ResumeCursor::resume_seq`].
    Disconnected,
}

/// What a session needs from the outside world.
pub trait SessionPort {
    /// Next decoded server frame; `None` once the socket is gone.
    async fn next_frame(&mut self) -> Option<WsServerMessage>;
    /// Send a frame; `false` when the socket refused it.
    async fn send(&mut self, frame: &WsClientMessage) -> bool;
    /// `GET /v1/events` page of up to `limit` events after `since`.
    async fn events_since(
        &mut self,
        since: u64,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, String>;
    /// The server's head as announced by Hello.
    fn hello(&mut self, server_seq: u64);
    /// Publish `events` and report the processed range ending at `covered_to`.
    fn deliver(&mut self, events: Vec<EventEnvelope>, covered_to: u64);
    fn set_conn_state(&mut self, state: ConnState);
}

fn subscribe(since_seq: u64) -> WsClientMessage {
    // `since_seq` makes the server cover what arrived after the last HTTP
    // page with a Snapshot, so the switch to the live stream leaves no gap.
    WsClientMessage::Subscribe {
        since_seq: Some(since_seq),
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
    }
}

/// HTTP catch-up: fills the gap since the previous session (or from 0 on the
/// very first connect) until the server returns less than a full page. An
/// error ends it early; the Snapshot pages cover the rest.
async fn catch_up_over_http<P: SessionPort>(port: &mut P, cursor: &mut ResumeCursor) {
    let mut catchup = Vec::new();
    loop {
        let since = cursor.resume_seq();
        match port.events_since(since, CATCHUP_PAGE_SIZE).await {
            Ok(page) => {
                let short_page = page.len() < CATCHUP_PAGE_SIZE;
                catchup.extend(cursor.accept(page));
                if short_page || cursor.resume_seq() == since {
                    break;
                }
            }
            Err(e) => {
                leptos::logging::warn!("[ws] catch-up failed: {e} — continuing with Snapshot");
                break;
            }
        }
    }
    if !catchup.is_empty() {
        port.deliver(catchup, cursor.resume_seq());
    }
}

/// Drive one connected socket until it ends.
pub async fn run_session<P: SessionPort>(port: &mut P, cursor: &mut ResumeCursor) -> SessionResult {
    match port.next_frame().await {
        Some(WsServerMessage::Hello { server_seq, .. }) => port.hello(server_seq),
        other => {
            leptos::logging::warn!("[ws] expected Hello, got {other:?}");
            return SessionResult::Disconnected;
        }
    }

    port.set_conn_state(ConnState::CatchingUp);
    catch_up_over_http(port, cursor).await;

    let mut expected_since = cursor.resume_seq();
    if !port.send(&subscribe(expected_since)).await {
        leptos::logging::warn!("[ws] failed to send Subscribe");
        return SessionResult::Disconnected;
    }

    // `Some` until a Snapshot closes the backlog; holds the live tail.
    let mut held_live: Option<Vec<EventEnvelope>> = Some(Vec::new());

    while let Some(frame) = port.next_frame().await {
        match frame {
            WsServerMessage::Event { envelope } => match held_live.as_mut() {
                Some(held) => held.push(envelope),
                None => {
                    let fresh = cursor.accept(vec![envelope]);
                    if !fresh.is_empty() {
                        port.deliver(fresh, cursor.resume_seq());
                    }
                }
            },
            WsServerMessage::Snapshot {
                since_seq,
                mut events,
                has_more,
                next_seq,
            } => {
                let Some(held) = held_live.as_mut().filter(|_| since_seq == expected_since) else {
                    leptos::logging::warn!(
                        "[ws] unexpected Snapshot since_seq={since_seq}, reconnecting"
                    );
                    return SessionResult::Disconnected;
                };
                if !has_more {
                    events.append(held);
                }
                let fresh = cursor.accept(events);
                if let Some(next) = next_seq {
                    cursor.advance(next);
                }
                if !fresh.is_empty() || next_seq.is_some() {
                    port.deliver(fresh, cursor.resume_seq());
                }
                if !has_more {
                    held_live = None;
                    port.set_conn_state(ConnState::Live);
                    continue;
                }
                let Some(next) = next_seq.filter(|next| *next > since_seq) else {
                    leptos::logging::warn!(
                        "[ws] Snapshot since_seq={since_seq} next_seq={next_seq:?} made no progress"
                    );
                    return SessionResult::Disconnected;
                };
                expected_since = next;
                if !port.send(&subscribe(next)).await {
                    leptos::logging::warn!("[ws] failed to request next Snapshot page");
                    return SessionResult::Disconnected;
                }
            }
            WsServerMessage::Resync { from_seq, dropped } => {
                leptos::logging::warn!("[ws] Resync from_seq={from_seq} dropped={dropped}");
                return SessionResult::Resync { from_seq };
            }
            WsServerMessage::Ping => {
                let _ = port.send(&WsClientMessage::Pong).await;
            }
            WsServerMessage::Error { code, message, .. } if held_live.is_some() => {
                leptos::logging::warn!("[ws] Subscribe refused: {code}: {message}");
                return SessionResult::Disconnected;
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

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};

    use daruma_domain::Actor;
    use daruma_events::Event;
    use daruma_shared::ProjectId;
    use futures_util::FutureExt;

    use super::*;

    /// Page size of the pinned server (`apps/server/src/ws.rs`, `SNAPSHOT_PAGE`).
    const SNAPSHOT_PAGE: usize = 1000;

    fn envelope(seq: u64) -> EventEnvelope {
        let mut env = EventEnvelope::new(
            Actor::user(),
            Event::ProjectDeleted {
                project_id: ProjectId::new(),
            },
        );
        env.seq = seq;
        env
    }

    /// The server side, same paging and boundary rules as the pinned server.
    /// Survives reconnects, like the log it models.
    struct Server {
        head: u64,
        /// Seqs the subscriber's filters hide from Snapshot and live frames.
        hidden: HashSet<u64>,
        /// Answers to `GET /v1/events`, front first; then it answers normally.
        http_script: VecDeque<Result<(), String>>,
        http_calls: Vec<u64>,
        subscribes: Vec<u64>,
    }

    impl Server {
        fn with_events(head: u64) -> Self {
            Self {
                head,
                hidden: HashSet::new(),
                http_script: VecDeque::new(),
                http_calls: Vec::new(),
                subscribes: Vec::new(),
            }
        }

        fn http_down(mut self, calls: usize) -> Self {
            self.http_script = (0..calls).map(|_| Err("HTTP 503".to_string())).collect();
            self
        }

        fn page(&self, since: u64, limit: usize) -> Vec<u64> {
            (since + 1..=self.head).take(limit).collect()
        }
    }

    /// What the viewer ended up with. Survives reconnects.
    #[derive(Default)]
    struct Viewer {
        seqs: Vec<u64>,
        states: Vec<ConnState>,
        /// `seqs.len()` each time the session reported Live.
        delivered_at_live: Vec<usize>,
        highest_reported: u64,
    }

    impl Viewer {
        fn assert_exactly(&self, expected: impl IntoIterator<Item = u64>) {
            let expected: Vec<u64> = expected.into_iter().collect();
            let first_diff = self.seqs.iter().zip(&expected).position(|(a, b)| a != b);
            assert!(
                self.seqs == expected,
                "every event exactly once, in order: got {} events, expected {}, first diff at {first_diff:?}",
                self.seqs.len(),
                expected.len(),
            );
        }

        fn went_live(&self) -> bool {
            self.states.contains(&ConnState::Live)
        }
    }

    /// One connection. The socket closes once its frames and steps run out.
    struct FakePort<'a> {
        server: &'a mut Server,
        viewer: &'a mut Viewer,
        frames: VecDeque<WsServerMessage>,
        /// Frames arriving one at a time once the queue is empty, for what
        /// happens after the session went quiet.
        later: VecDeque<WsServerMessage>,
        /// `(n, count)`: `count` events commit right after the n-th Snapshot
        /// is sent, so they travel behind it while the viewer pages on.
        commit_after_snapshot: Option<(usize, u64)>,
        /// Subscribes the server answers; later ones get no reply.
        replies_left: Option<usize>,
        /// Sends accepted before the socket refuses them.
        sends_left: Option<usize>,
        snapshots_sent: usize,
    }

    impl<'a> FakePort<'a> {
        fn connect(server: &'a mut Server, viewer: &'a mut Viewer) -> Self {
            let hello = WsServerMessage::Hello {
                server_seq: server.head,
                capabilities: Vec::new(),
            };
            Self {
                server,
                viewer,
                frames: VecDeque::from([hello]),
                later: VecDeque::new(),
                commit_after_snapshot: None,
                replies_left: None,
                sends_left: None,
                snapshots_sent: 0,
            }
        }

        fn commit_after_snapshot(mut self, snapshot: usize, count: u64) -> Self {
            self.commit_after_snapshot = Some((snapshot, count));
            self
        }

        fn lose_replies_after(mut self, replies: usize) -> Self {
            self.replies_left = Some(replies);
            self
        }

        fn refuse_sends_after(mut self, sends: usize) -> Self {
            self.sends_left = Some(sends);
            self
        }

        fn then(mut self, frames: impl IntoIterator<Item = WsServerMessage>) -> Self {
            self.later.extend(frames);
            self
        }

        fn commit(&mut self, count: u64) {
            for _ in 0..count {
                self.server.head += 1;
                let seq = self.server.head;
                if !self.server.hidden.contains(&seq) {
                    self.frames.push_back(WsServerMessage::Event {
                        envelope: envelope(seq),
                    });
                }
            }
        }

        fn run(mut self, cursor: &mut ResumeCursor) -> SessionResult {
            // Every fake await is ready at once, so one poll runs it to the end.
            run_session(&mut self, cursor)
                .now_or_never()
                .expect("fake session never waits")
        }
    }

    impl SessionPort for FakePort<'_> {
        async fn next_frame(&mut self) -> Option<WsServerMessage> {
            self.frames.pop_front().or_else(|| self.later.pop_front())
        }

        async fn send(&mut self, frame: &WsClientMessage) -> bool {
            if let Some(left) = self.sends_left.as_mut() {
                if *left == 0 {
                    return false;
                }
                *left -= 1;
            }
            let WsClientMessage::Subscribe {
                since_seq: Some(since),
                ..
            } = *frame
            else {
                return true;
            };
            self.server.subscribes.push(since);
            assert!(
                self.server.subscribes.len() < 100,
                "pagination does not converge"
            );
            if let Some(left) = self.replies_left.as_mut() {
                if *left == 0 {
                    return true;
                }
                *left -= 1;
            }
            let page = self.server.page(since, SNAPSHOT_PAGE);
            let shown = page
                .iter()
                .filter(|seq| !self.server.hidden.contains(seq))
                .map(|seq| envelope(*seq))
                .collect();
            self.frames.push_back(WsServerMessage::Snapshot {
                since_seq: since,
                events: shown,
                has_more: page.len() >= SNAPSHOT_PAGE,
                next_seq: page.last().copied(),
            });
            self.snapshots_sent += 1;
            if let Some((snapshot, count)) = self.commit_after_snapshot {
                if snapshot == self.snapshots_sent {
                    self.commit(count);
                }
            }
            true
        }

        async fn events_since(
            &mut self,
            since: u64,
            limit: usize,
        ) -> Result<Vec<EventEnvelope>, String> {
            self.server.http_calls.push(since);
            self.server.http_script.pop_front().transpose()?;
            let page = self.server.page(since, limit);
            Ok(page.into_iter().map(envelope).collect())
        }

        fn hello(&mut self, _server_seq: u64) {}

        fn deliver(&mut self, events: Vec<EventEnvelope>, covered_to: u64) {
            self.viewer.seqs.extend(events.iter().map(|env| env.seq));
            self.viewer.highest_reported = self.viewer.highest_reported.max(covered_to);
        }

        fn set_conn_state(&mut self, state: ConnState) {
            if state == ConnState::Live {
                self.viewer.delivered_at_live.push(self.viewer.seqs.len());
            }
            self.viewer.states.push(state);
        }
    }

    fn live_frame(seq: u64) -> WsServerMessage {
        WsServerMessage::Event {
            envelope: envelope(seq),
        }
    }

    #[test]
    fn failed_http_then_partial_snapshot_and_live_event_keeps_the_whole_history() {
        let mut server = Server::with_events(1500).http_down(1);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        FakePort::connect(&mut server, &mut viewer)
            .commit_after_snapshot(1, 1)
            .run(&mut cursor);

        viewer.assert_exactly(1..=1501);
        assert_eq!(viewer.delivered_at_live, [1501]);
        assert_eq!(server.subscribes, [0, 1000]);
        assert_eq!(cursor.resume_seq(), 1501);
    }

    #[test]
    fn reconnect_mid_backlog_resumes_at_the_page_end_not_at_the_live_event() {
        let mut server = Server::with_events(1500).http_down(1);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        // The live event arrives, then the socket dies before the next page.
        let result = FakePort::connect(&mut server, &mut viewer)
            .commit_after_snapshot(1, 1)
            .lose_replies_after(1)
            .run(&mut cursor);

        assert!(matches!(result, SessionResult::Disconnected));
        assert!(!viewer.went_live(), "a dropped backlog is not Live");
        viewer.assert_exactly(1..=1000);
        assert_eq!(viewer.highest_reported, 1000);
        assert_eq!(cursor.resume_seq(), 1000);

        // HTTP is back: the next connect fetches exactly the missing range.
        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        assert_eq!(server.http_calls, [0, 1000, 1500]);
        viewer.assert_exactly(1..=1501);
        assert_eq!(viewer.delivered_at_live, [1501]);
    }

    #[test]
    fn socket_loss_between_pages_is_not_live_and_loses_nothing() {
        let mut server = Server::with_events(2500).http_down(1);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        let result = FakePort::connect(&mut server, &mut viewer)
            .refuse_sends_after(1)
            .run(&mut cursor);

        assert!(matches!(result, SessionResult::Disconnected));
        assert_eq!(viewer.states, [ConnState::CatchingUp]);
        assert_eq!(cursor.resume_seq(), 1000);

        // HTTP is still down, so the second session pages over the socket.
        server.http_script = VecDeque::from([Err("HTTP 503".to_string())]);
        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        assert_eq!(server.subscribes, [0, 1000, 2000]);
        viewer.assert_exactly(1..=2500);
        assert_eq!(viewer.delivered_at_live, [2500]);
    }

    #[test]
    fn error_after_a_good_http_page_continues_from_that_page() {
        let mut server = Server::with_events(1800);
        server.http_script = VecDeque::from([Ok(()), Err("HTTP 503".to_string())]);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        assert_eq!(server.http_calls, [0, 500]);
        assert_eq!(server.subscribes, [500, 1500]);
        viewer.assert_exactly(1..=1800);
        assert_eq!(viewer.delivered_at_live, [1800]);
    }

    #[test]
    fn disconnect_after_a_good_http_page_resumes_from_that_page() {
        let mut server = Server::with_events(1800);
        server.http_script = VecDeque::from([Ok(()), Err("HTTP 503".to_string())]);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        let result = FakePort::connect(&mut server, &mut viewer)
            .refuse_sends_after(0)
            .run(&mut cursor);

        assert!(matches!(result, SessionResult::Disconnected));
        assert_eq!(cursor.resume_seq(), 500);
        assert!(!viewer.went_live());

        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        assert_eq!(server.http_calls[2], 500);
        viewer.assert_exactly(1..=1800);
    }

    #[test]
    fn live_events_during_pagination_are_held_and_delivered_once() {
        // They commit behind the first page; the next Subscribe moves the
        // boundary over them, so the closing page carries them too.
        let mut server = Server::with_events(1500).http_down(1);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        FakePort::connect(&mut server, &mut viewer)
            .commit_after_snapshot(1, 3)
            .run(&mut cursor);

        viewer.assert_exactly(1..=1503);
        assert_eq!(viewer.delivered_at_live, [1503]);
    }

    #[test]
    fn held_event_past_the_closing_page_is_delivered_when_the_backlog_closes() {
        let mut server = Server::with_events(1200).http_down(1);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();
        let mut port = FakePort::connect(&mut server, &mut viewer);
        port.frames.push_back(live_frame(1201));

        port.run(&mut cursor);

        viewer.assert_exactly(1..=1201);
        assert_eq!(viewer.delivered_at_live, [1201]);
        assert_eq!(cursor.resume_seq(), 1201);
    }

    #[test]
    fn empty_filtered_page_with_more_keeps_paging() {
        let mut server = Server::with_events(2500).http_down(1);
        server.hidden = (1..=2000).collect();
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        assert_eq!(server.subscribes, [0, 1000, 2000]);
        viewer.assert_exactly(2001..=2500);
        assert_eq!(viewer.delivered_at_live, [500]);
        assert_eq!(cursor.resume_seq(), 2500);
    }

    #[test]
    fn filtered_tail_still_moves_the_cursor_to_the_page_end() {
        let mut server = Server::with_events(1500).http_down(1);
        server.hidden = (1401..=1500).collect();
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        viewer.assert_exactly(1..=1400);
        assert_eq!(cursor.resume_seq(), 1500);
    }

    #[test]
    fn page_of_exactly_the_limit_ends_on_the_next_empty_page() {
        let mut server = Server::with_events(1000).http_down(1);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        assert_eq!(server.subscribes, [0, 1000]);
        viewer.assert_exactly(1..=1000);
        assert!(viewer.went_live());
    }

    #[test]
    fn has_more_without_progress_reconnects_instead_of_looping() {
        for next_seq in [None, Some(0)] {
            let mut server = Server::with_events(0);
            let mut viewer = Viewer::default();
            let mut cursor = ResumeCursor::default();
            let mut port = FakePort::connect(&mut server, &mut viewer);
            port.frames.push_back(WsServerMessage::Snapshot {
                since_seq: 0,
                events: Vec::new(),
                has_more: true,
                next_seq,
            });

            let result = port.run(&mut cursor);

            assert!(matches!(result, SessionResult::Disconnected));
            assert!(!viewer.went_live());
            assert_eq!(server.subscribes, [0], "no second request for a stuck page");
        }
    }

    #[test]
    fn refused_subscribe_is_not_live() {
        let mut server = Server::with_events(0);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();
        let mut port = FakePort::connect(&mut server, &mut viewer);
        port.frames.push_back(WsServerMessage::Error {
            code: "forbidden".to_string(),
            message: "token lacks any subscribe capability".to_string(),
            request_id: None,
        });

        let result = port.run(&mut cursor);

        assert!(matches!(result, SessionResult::Disconnected));
        assert_eq!(viewer.states, [ConnState::CatchingUp]);
    }

    #[test]
    fn snapshot_for_another_cursor_is_refused() {
        let mut server = Server::with_events(0);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();
        let mut port = FakePort::connect(&mut server, &mut viewer);
        port.frames.push_back(WsServerMessage::Snapshot {
            since_seq: 7,
            events: vec![envelope(8)],
            has_more: false,
            next_seq: Some(8),
        });

        let result = port.run(&mut cursor);

        assert!(matches!(result, SessionResult::Disconnected));
        assert!(viewer.seqs.is_empty());
        assert!(!viewer.went_live());
    }

    #[test]
    fn ordinary_startup_goes_live_after_one_snapshot() {
        let mut server = Server::with_events(700);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        FakePort::connect(&mut server, &mut viewer)
            .commit_after_snapshot(1, 2)
            .run(&mut cursor);

        assert_eq!(server.http_calls, [0, 500]);
        assert_eq!(server.subscribes, [700]);
        assert_eq!(
            viewer.delivered_at_live,
            [700],
            "Live precedes the live tail"
        );
        viewer.assert_exactly(1..=702);
        assert_eq!(viewer.highest_reported, 702);
    }

    #[test]
    fn ordinary_reconnect_continues_after_the_last_live_event() {
        let mut server = Server::with_events(10);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();
        FakePort::connect(&mut server, &mut viewer)
            .commit_after_snapshot(1, 2)
            .run(&mut cursor);
        assert_eq!(cursor.resume_seq(), 12);

        server.head = 15;
        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        assert_eq!(server.http_calls.last(), Some(&12));
        assert_eq!(server.subscribes, [10, 15]);
        viewer.assert_exactly(1..=15);
    }

    #[test]
    fn repeated_live_frame_is_delivered_once() {
        let mut server = Server::with_events(3);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();

        FakePort::connect(&mut server, &mut viewer)
            .then([live_frame(3), live_frame(4), live_frame(4)])
            .run(&mut cursor);

        viewer.assert_exactly(1..=4);
    }

    #[test]
    fn resync_rewinds_and_redelivers_nothing() {
        let mut server = Server::with_events(5);
        let mut viewer = Viewer::default();
        let mut cursor = ResumeCursor::default();
        let result = FakePort::connect(&mut server, &mut viewer)
            .then([WsServerMessage::Resync {
                from_seq: 2,
                dropped: 1,
            }])
            .run(&mut cursor);
        let SessionResult::Resync { from_seq } = result else {
            panic!("expected Resync");
        };

        cursor.rewind(from_seq);
        assert_eq!(cursor.resume_seq(), 2);
        FakePort::connect(&mut server, &mut viewer).run(&mut cursor);

        assert_eq!(server.http_calls.last(), Some(&2));
        viewer.assert_exactly(1..=5);
    }

    #[test]
    fn resync_past_the_processed_range_does_not_skip_it() {
        let mut cursor = ResumeCursor::default();
        cursor.advance(1000);

        cursor.rewind(1501);

        assert_eq!(cursor.resume_seq(), 1000);
    }
}
