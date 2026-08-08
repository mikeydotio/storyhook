//! The daemon's change feed.
//!
//! Every connected `/api/events` client subscribes here, and everything that
//! changes the store publishes here. Several publishers, deliberately:
//!
//! 1. **The request boundary**, immediately after a mutating request commits.
//!    Not inside the transaction — a subscriber woken while the writer still
//!    holds the write lock would re-read the *previous* state, which is the
//!    stale-dashboard bug in a new costume. Covers both routes
//!    `daemon::serve::route_job_inner` dispatches to: the dashboard's own REST
//!    mutation routes publish a precise `Changed` signal they already compute
//!    (`rest::route`'s `Routed::changed`); `POST /api/v1/invoke` — the *only*
//!    way an ordinary `story` command reaches the store — carries no such
//!    signal of its own, so it calls
//!    [`crate::daemon::watch::ChangeWatcher::notice`] instead, the same
//!    store-wide diff `poll_change_token` below runs on a schedule (SH-202).
//!    **Deliberately not called from the REST arm too** — every mutating REST
//!    route already has exhaustive `Changed` coverage by construction, and a
//!    `notice()` call there was found, empirically, to risk attributing an
//!    unrelated out-of-band commit to the wrong response and colliding with
//!    the coalescing window below (SH-202's own council record; the surviving
//!    hazard is filed as SH-216).
//! 2. **`poll_change_token`** (`daemon::serve`), a low-frequency safety net
//!    over SQLite's `PRAGMA data_version`, which changes whenever *another
//!    connection* commits. That is how a write this
//!    daemon did not serve at its own request boundary at all — a `story tui`
//!    session on the same store, a second machine, a `sqlite3` prompt —
//!    reaches a browser the daemon never heard the write from.
//! 3. **A dashboard dispatch's completion** (`crate::api::dispatch`), so a tab
//!    watching a `script --project … dispatch` run sees the story it moved
//!    without polling that endpoint itself.
//! 4. **[`Change::Ping`]**, on a fixed interval, purely to let a client detect
//!    a connection that died without a clean close (SH-145).
//! 5. **[`Change::Reload`]**, once, immediately before this daemon answers a
//!    shutdown request — so a client about to lose its stream reconnects on
//!    purpose rather than after its own retry timer expires.
//!
//! This replaces a filesystem watcher, and it is strictly more reliable than
//! one. The watcher observed a directory of story files and provably missed
//! project *configuration* changes, because the directory holding `states.toml`
//! also held a SQLite archive whose sidecar files were rewritten by reads — so
//! watching it turned every client's refetch into a change event for every other
//! client, and the only way out was not to watch it at all.
//!
//! # Backpressure
//!
//! A subscriber's queue is bounded. A client that stops reading — a laptop that
//! slept with the tab open — must not be able to grow the daemon's memory, and a
//! client that falls behind does not want the backlog anyway: it wants to know
//! that it is behind. So an overflowing queue is *collapsed* to a single
//! [`Change::Resync`] and the drop is counted. Silent loss is the one thing not
//! on the menu.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How many undelivered messages one subscriber may hold before its queue is
/// collapsed to a resync.
///
/// Small on purpose. These are "something changed, refetch" notices, not data,
/// so a deep queue buys a slow client nothing but latency: by the time it drains
/// the tenth notice the first nine describe states that no longer exist.
const QUEUE_CAPACITY: usize = 32;

/// How long two identical changes have to be apart to be published twice.
///
/// The request boundary and the `data_version` poller can both notice the same
/// commit, and a burst of writes to one project — a `story bulk-update`, a
/// decompose — is one thing a browser needs to hear about, not twenty.
const COALESCE_WINDOW: Duration = Duration::from_millis(200);

/// A message pushed to every connected `/api/events` client.
///
/// Deliberately coarse — never a server-computed diff — so the daemon stays as
/// stateless about UI concerns as every endpoint it serves. The browser holds
/// the last snapshot it fetched and diffs a fresh one against it to decide what
/// actually moved (see `diffSnapshots` in `web_dashboard.html`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// One project's stories or configuration changed; the payload is the
    /// project slug, which is what a dashboard URL names.
    Project(String),
    /// The set of projects changed — one was registered or deregistered.
    Catalog,
    /// This subscriber missed messages and must refetch everything.
    ///
    /// Published rather than logged: a client that silently missed an update is
    /// a client showing something that is not true, which is precisely the
    /// failure this feed exists to prevent.
    Resync,
    /// The daemon is shutting down and the client should reconnect.
    ///
    /// Sent before a version-skew restart so a browser reconnects to the new
    /// daemon on purpose rather than after its own retry timer expires.
    Reload,
    /// Keep-alive. A failed write while sending this is how a connection
    /// that vanished without a clean close (sleep, network drop) is
    /// detected and its subscriber pruned — *when the write actually
    /// fails*, which a silently dead connection may never make happen: a
    /// browser's TCP stack can keep accepting small, infrequent writes into
    /// its local receive buffer indefinitely without them ever crossing a
    /// link that no longer exists. A real named event, rather than a bare
    /// SSE comment, so the client side has the same signal to act on —
    /// `web_dashboard.html`'s `sseWatchdog` treats the *absence* of one
    /// arriving as the proof the request boundary and the server-side write
    /// failure cannot supply (SH-145).
    Ping,
}

impl Change {
    /// This change as a complete SSE message: event name, `data:` line, and the
    /// blank line that tells an `EventSource` the message is over.
    pub fn to_sse(&self) -> String {
        match self {
            Change::Ping => "event: ping\ndata: {}\n\n".to_string(),
            Change::Catalog => "event: repos-changed\ndata: {}\n\n".to_string(),
            Change::Project(slug) => format!(
                "event: repo-changed\ndata: {}\n\n",
                serde_json::json!({ "repo_id": slug })
            ),
            Change::Resync => "event: resync\ndata: {}\n\n".to_string(),
            Change::Reload => "event: reload\ndata: {}\n\n".to_string(),
        }
    }

    /// The inverse of [`Self::to_sse`], for the one caller that is this
    /// process rather than a browser: [`crate::daemon::subscribe`].
    ///
    /// Takes one already-decoded SSE event — everything between two blank
    /// lines, `event:`/`data:` lines included — not a byte stream; de-chunking
    /// and frame-splitting are [`crate::daemon::subscribe`]'s job, so that this
    /// function stays exactly what [`Self::to_sse`] is: pure formatting, with
    /// no I/O and no partial-read state to get wrong. Unrecognised event names
    /// answer `None` rather than erroring, because a client running behind the
    /// daemon it is subscribed to must survive an event name it does not know
    /// yet, the same way an unknown JSON field would be ignored rather than
    /// refused.
    pub fn from_sse(frame: &str) -> Option<Change> {
        let mut event = None;
        let mut data = None;
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("event: ") {
                event = Some(rest);
            } else if let Some(rest) = line.strip_prefix("data: ") {
                data = Some(rest);
            }
        }
        match event? {
            "ping" => Some(Change::Ping),
            "repos-changed" => Some(Change::Catalog),
            "repo-changed" => {
                let value: serde_json::Value = serde_json::from_str(data?).ok()?;
                let slug = value.get("repo_id")?.as_str()?;
                Some(Change::Project(slug.to_string()))
            }
            "resync" => Some(Change::Resync),
            "reload" => Some(Change::Reload),
            _ => None,
        }
    }
}

/// One subscriber's mailbox.
struct Slot {
    queue: Mutex<VecDeque<Change>>,
    signal: Condvar,
    dropped: AtomicU64,
}

impl Slot {
    fn push(&self, change: Change) {
        let mut queue = self.queue.lock().unwrap();
        if queue.len() >= QUEUE_CAPACITY {
            // Collapse rather than trim. A client this far behind cannot use
            // the backlog; what it can use is one instruction to start over.
            let lost = queue.len() as u64;
            queue.clear();
            queue.push_back(Change::Resync);
            self.dropped.fetch_add(lost, Ordering::Relaxed);
        } else {
            queue.push_back(change);
        }
        self.signal.notify_one();
    }

    /// The next change, waiting up to `timeout` for one to arrive.
    fn recv(&self, timeout: Duration) -> Option<Change> {
        let mut queue = self.queue.lock().unwrap();
        loop {
            if let Some(change) = queue.pop_front() {
                return Some(change);
            }
            let (next, wait) = self.signal.wait_timeout(queue, timeout).unwrap();
            queue = next;
            if wait.timed_out() && queue.is_empty() {
                return None;
            }
        }
    }
}

/// A subscription, held for the life of one `/api/events` connection.
///
/// Unsubscribes on drop, so a connection that ends — cleanly, by panic, or by
/// early return — can never leak its slot.
pub struct Subscription {
    bus: ChangeBus,
    id: u64,
    slot: Arc<Slot>,
}

impl Subscription {
    /// The next change, or `None` if `timeout` elapses with nothing to report.
    pub fn recv(&self, timeout: Duration) -> Option<Change> {
        self.slot.recv(timeout)
    }

    /// How many messages this subscriber has lost to a full queue.
    pub fn dropped(&self) -> u64 {
        self.slot.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.bus.unsubscribe(self.id);
    }
}

#[derive(Default)]
struct BusState {
    next_id: u64,
    subscribers: Vec<(u64, Arc<Slot>)>,
    /// The last time each distinct change was published, for coalescing.
    recent: Vec<(Change, Instant)>,
    /// Total messages dropped across every subscriber this daemon has served,
    /// including ones that have since disconnected.
    dropped: u64,
}

/// The live set of connected `/api/events` clients, shared — every clone refers
/// to the same underlying list — between the request boundary, the
/// `data_version` poller, the heartbeat, and every connection handler.
#[derive(Clone, Default)]
pub struct ChangeBus {
    inner: Arc<Mutex<BusState>>,
}

impl ChangeBus {
    /// A bus with no subscribers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new subscriber.
    pub fn subscribe(&self) -> Subscription {
        let slot = Arc::new(Slot {
            queue: Mutex::new(VecDeque::new()),
            signal: Condvar::new(),
            dropped: AtomicU64::new(0),
        });
        let mut state = self.inner.lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;
        state.subscribers.push((id, Arc::clone(&slot)));
        drop(state);
        Subscription {
            bus: self.clone(),
            id,
            slot,
        }
    }

    fn unsubscribe(&self, id: u64) {
        let mut state = self.inner.lock().unwrap();
        if let Some(index) = state.subscribers.iter().position(|(sub, _)| *sub == id) {
            let (_, slot) = state.subscribers.remove(index);
            // Roll the departing subscriber's losses into the daemon-wide
            // counter, so `dropped()` describes the whole process rather than
            // whoever happens to be connected right now.
            state.dropped += slot.dropped.load(Ordering::Relaxed);
        }
    }

    /// Sends `change` to every subscriber, unless an identical change was
    /// published within [`COALESCE_WINDOW`].
    ///
    /// [`Change::Ping`] is never coalesced: its whole purpose is to be written
    /// on a schedule so that a dead connection is discovered.
    pub fn publish(&self, change: Change) {
        let mut state = self.inner.lock().unwrap();
        if !matches!(change, Change::Ping) {
            let now = Instant::now();
            state
                .recent
                .retain(|(_, at)| now.duration_since(*at) < COALESCE_WINDOW);
            if state.recent.iter().any(|(seen, _)| *seen == change) {
                return;
            }
            state.recent.push((change.clone(), now));
        }
        for (_, slot) in &state.subscribers {
            slot.push(change.clone());
        }
    }

    /// Number of connected clients, so a publisher can skip work when nobody is
    /// listening.
    pub fn subscriber_count(&self) -> usize {
        self.inner.lock().unwrap().subscribers.len()
    }

    /// How many messages this daemon has dropped, over its whole life.
    ///
    /// Observability, not decoration: a non-zero count means some client was
    /// told to resync, and a count that climbs means the feed is producing
    /// faster than a browser can drain it.
    pub fn dropped(&self) -> u64 {
        let state = self.inner.lock().unwrap();
        state.dropped
            + state
                .subscribers
                .iter()
                .map(|(_, slot)| slot.dropped.load(Ordering::Relaxed))
                .sum::<u64>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANT: Duration = Duration::from_millis(50);

    #[test]
    fn a_published_change_reaches_every_subscriber() {
        let bus = ChangeBus::new();
        let a = bus.subscribe();
        let b = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);

        bus.publish(Change::Project("app".into()));
        assert_eq!(a.recv(INSTANT), Some(Change::Project("app".into())));
        assert_eq!(b.recv(INSTANT), Some(Change::Project("app".into())));
    }

    #[test]
    fn a_dropped_subscription_stops_receiving() {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        drop(subscription);
        assert_eq!(bus.subscriber_count(), 0);
        // Publishing to nobody must not panic or block.
        bus.publish(Change::Catalog);
    }

    #[test]
    fn an_identical_change_within_the_window_is_published_once() {
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();
        bus.publish(Change::Project("app".into()));
        bus.publish(Change::Project("app".into()));
        assert_eq!(
            subscriber.recv(INSTANT),
            Some(Change::Project("app".into()))
        );
        assert_eq!(
            subscriber.recv(INSTANT),
            None,
            "the second identical change must be coalesced, not queued"
        );
    }

    #[test]
    fn different_projects_are_never_coalesced_into_one_another() {
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();
        bus.publish(Change::Project("app".into()));
        bus.publish(Change::Project("other".into()));
        assert_eq!(
            subscriber.recv(INSTANT),
            Some(Change::Project("app".into()))
        );
        assert_eq!(
            subscriber.recv(INSTANT),
            Some(Change::Project("other".into()))
        );
    }

    #[test]
    fn a_heartbeat_is_never_coalesced() {
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();
        bus.publish(Change::Ping);
        bus.publish(Change::Ping);
        assert_eq!(subscriber.recv(INSTANT), Some(Change::Ping));
        assert_eq!(
            subscriber.recv(INSTANT),
            Some(Change::Ping),
            "a coalesced heartbeat stops detecting dead connections, which is \
             the only thing it is for"
        );
    }

    /// The property the whole bounded-queue design exists for: a client that
    /// stops reading cannot grow the daemon without bound, and what it is told
    /// when it comes back is "start over", not a stale replay.
    #[test]
    fn an_overflowing_queue_collapses_to_a_resync_and_counts_what_it_lost() {
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();
        for n in 0..QUEUE_CAPACITY + 10 {
            bus.publish(Change::Project(format!("project-{n}")));
        }

        assert_eq!(
            subscriber.recv(INSTANT),
            Some(Change::Resync),
            "the collapsed queue must lead with the instruction to refetch"
        );
        assert!(
            subscriber.dropped() > 0,
            "a drop that nobody can count is a silent one"
        );
        assert!(
            bus.dropped() >= subscriber.dropped(),
            "the daemon-wide counter must include every live subscriber's losses"
        );
    }

    #[test]
    fn losses_survive_the_subscriber_that_suffered_them() {
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();
        for n in 0..QUEUE_CAPACITY + 10 {
            bus.publish(Change::Project(format!("project-{n}")));
        }
        let lost = subscriber.dropped();
        assert!(lost > 0);
        drop(subscriber);
        assert_eq!(
            bus.dropped(),
            lost,
            "a counter that resets when the client disconnects cannot answer \
             `is this daemon dropping messages`"
        );
    }

    #[test]
    fn recv_reports_nothing_rather_than_blocking_forever() {
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();
        assert_eq!(subscriber.recv(Duration::from_millis(10)), None);
    }

    #[test]
    fn every_change_renders_a_complete_sse_message() {
        for change in [
            Change::Project("app".into()),
            Change::Catalog,
            Change::Resync,
            Change::Reload,
            Change::Ping,
        ] {
            let frame = change.to_sse();
            assert!(
                frame.ends_with("\n\n"),
                "{change:?} must end in the blank line that terminates an SSE \
                 message; got {frame:?}"
            );
        }
        assert!(Change::Project("app".into()).to_sse().contains("\"app\""));
    }

    #[test]
    fn every_change_round_trips_through_sse() {
        for change in [
            Change::Project("app".into()),
            Change::Catalog,
            Change::Resync,
            Change::Reload,
            Change::Ping,
        ] {
            assert_eq!(
                Change::from_sse(&change.to_sse()),
                Some(change.clone()),
                "{change:?} did not survive encoding and decoding"
            );
        }
    }

    #[test]
    fn from_sse_ignores_an_event_it_does_not_recognise() {
        assert_eq!(
            Change::from_sse("event: some-future-event\ndata: {}\n\n"),
            None
        );
    }

    #[test]
    fn from_sse_ignores_the_connected_sentinel() {
        // What `serve_sse` writes before the first real change -- not a
        // `Change` at all, and `subscribe.rs` must treat it as "the
        // connection is up" rather than as a change to report.
        assert_eq!(Change::from_sse("retry: 3000\n: connected\n\n"), None);
    }
}
