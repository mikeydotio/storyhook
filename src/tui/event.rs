use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, MouseEvent};

use crate::daemon::lifecycle::DaemonInfo;
use crate::daemon::subscribe::Subscriber;
use crate::env::Environment;

/// Events flowing through the TUI main loop.
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    DataChanged,
    Tick,
}

/// How long one [`Subscriber::poll`] call may block before the change thread
/// re-checks `stop`. Independent of the daemon's own heartbeat interval — this
/// only bounds shutdown latency, the same role it played when this constant
/// bounded a `PRAGMA data_version` poll interval instead.
const POLL_BUDGET: Duration = Duration::from_millis(250);

/// How long the first change of a burst holds the window open for the others
/// before the interface is told to reload once, for all of them (SH-247).
///
/// The shape is the browser's, deliberately: `web_dashboard.html`'s
/// `scheduleDataFetch` arms a timer on the first event and refetches when it
/// fires, absorbing everything that lands inside it. Two properties come with
/// that shape and both are load-bearing. It throttles on the **trailing** edge,
/// so every absorbed change is still followed by a reload — dropping the
/// *later* of two changes is precisely the defect SH-216 was filed for, and
/// this must never become that. And the window is measured from the first
/// change rather than restarted by each one, so a store somebody writes to
/// without pause still reloads on a fixed cadence instead of starving.
///
/// The number is this interface's own, not the browser's 150ms by inheritance:
///
/// * **Floor** — it has to be wider than the gaps inside a real burst, or it
///   collapses nothing. A `story` command against a warm daemon takes 10–50ms
///   end to end (measured), so a shell loop of them publishes about that far
///   apart, and the eight concurrent request-boundary publishes `DISPATCHERS`
///   caps land within a few milliseconds of each other.
/// * **Ceiling** — it is added to the latency of every change, a lone one
///   included. The TUI is already quantised at 250ms by its own tick thread, so
///   a window inside that adds no delay distinguishable from the cadence the
///   interface has anyway.
///
/// That it lands on the same number the browser picked is a coincidence worth
/// keeping rather than the reason for it: one absorption window across both
/// clients of one feed is one number to reason about.
const COALESCE_WINDOW: Duration = Duration::from_millis(150);

/// Manages background threads that produce events for the main loop.
///
/// Three threads:
/// 1. Input: `crossterm::event::poll(100ms)` + `read()`
/// 2. Change: the daemon's change feed, subscribed to, coalesced over
///    [`COALESCE_WINDOW`]
/// 3. Tick: a 250ms heartbeat, which drives notification expiry
pub struct EventSource {
    stop: Arc<AtomicBool>,
    _input_handle: JoinHandle<()>,
    _change_handle: JoinHandle<()>,
    _tick_handle: JoinHandle<()>,
}

impl EventSource {
    /// Create the event source and return the receiver for the main loop.
    ///
    /// # Why this watches the daemon rather than the filesystem
    ///
    /// It used to run a `notify` watcher over `.storyhook/`, which is where the
    /// data was. The data is in one database now, and a filesystem watcher over
    /// a repository would report a build artifact and miss a story created from
    /// another checkout — the opposite of what it is for.
    ///
    /// # Why this subscribes to the daemon's feed now, having once refused to
    ///
    /// This function used to poll `PRAGMA data_version` on a connection of its
    /// own, and the reasoning for that (kept here rather than deleted, because
    /// the counter-argument is worth being able to check) was:
    ///
    /// - *"It works with no daemon running at all."* True when the TUI held
    ///   its own store handle; no longer relevant once it does not (SH-150) —
    ///   `story tui` now needs a daemon for every other read and write it
    ///   makes, the same as every other command since SH-114.
    /// - *"A connection that can drop learns the same fact one layer further
    ///   away."* True of a connection that silently goes quiet. [`Subscriber`]
    ///   is built specifically not to: it watches the daemon's own heartbeat
    ///   and reconnects rather than staying silent, and a reconnect always
    ///   costs exactly one [`Event::DataChanged`] — the same "refetch" a
    ///   missed poll interval never had to announce, only slower to notice.
    ///
    /// What it gains: one `PRAGMA` poll per `story` process on the machine —
    /// the daemon's own [`crate::daemon::serve::poll_change_token`] — instead
    /// of one per open TUI, and a change reported as soon as the daemon
    /// publishes it rather than up to [`POLL_BUDGET`] later.
    ///
    /// `daemon` is the caller's already-resolved [`DaemonInfo`] — spent on the
    /// very first connection so this does not pay for a second
    /// `lifecycle::ensure` on the startup path; see [`Subscriber::new`].
    pub fn new(env: &Environment, daemon: &DaemonInfo) -> (Self, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        let input_handle = Self::spawn_input_thread(tx.clone(), Arc::clone(&stop));
        let change_handle = Self::spawn_change_thread(env, daemon, tx.clone(), Arc::clone(&stop));
        let tick_handle = Self::spawn_tick_thread(tx, Arc::clone(&stop));

        let source = Self {
            stop,
            _input_handle: input_handle,
            _change_handle: change_handle,
            _tick_handle: tick_handle,
        };

        (source, rx)
    }

    /// Signal all background threads to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    fn spawn_input_thread(tx: Sender<Event>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                // Poll with 100ms timeout so we can check the stop flag
                let available = crossterm::event::poll(Duration::from_millis(100));
                match available {
                    Ok(true) => {
                        if let Ok(ct_event) = crossterm::event::read() {
                            let event = match ct_event {
                                crossterm::event::Event::Key(key) => Some(Event::Key(key)),
                                crossterm::event::Event::Mouse(mouse) => Some(Event::Mouse(mouse)),
                                crossterm::event::Event::Resize(w, h) => Some(Event::Resize(w, h)),
                                _ => None,
                            };
                            if let Some(e) = event
                                && tx.send(e).is_err()
                            {
                                break;
                            }
                        }
                    }
                    Ok(false) => {} // timeout, loop again
                    Err(_) => break,
                }
            }
        })
    }

    /// Watches the daemon's change feed for writes made by anything else —
    /// another checkout, a `story` command in a second terminal, the
    /// dashboard — and reports at most one [`Event::DataChanged`] per
    /// [`COALESCE_WINDOW`]; [`pump_changes`] holds the rule.
    ///
    /// # The one property that must survive every rewrite of this function
    ///
    /// **The subscription is established here, on the caller's thread, before
    /// this function returns — never inside the spawned closure.** This is
    /// SH-140's fix, carried forward onto a different mechanism: that story's
    /// bug was a `PRAGMA data_version` baseline read *inside* `thread::spawn`'s
    /// closure, so a write landing in the gap between spawning and the
    /// closure actually running was folded into the baseline and could never
    /// be reported — not late, but never. [`Subscriber::connect`] blocks until
    /// the daemon's own `bus.subscribe()` has run, which is the equivalent
    /// guarantee for a push feed: a write ordered after this function returns
    /// cannot predate the subscription, so it cannot land in an unreportable
    /// gap. `tui::app::run` depends on this too — it subscribes before it
    /// loads the initial snapshot, for the same reason.
    ///
    /// A daemon this cannot reach at all means no live updates — the TUI
    /// still works, and `r` still reloads — so that failure is silent by
    /// design (`Subscriber::connect`'s `Result` is discarded here) rather
    /// than by neglect: the background thread's own reconnect loop keeps
    /// trying, and its first success reports one [`Event::DataChanged`].
    fn spawn_change_thread(
        env: &Environment,
        daemon: &DaemonInfo,
        tx: Sender<Event>,
        stop: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        let heartbeat = crate::daemon::serve::heartbeat_interval();
        let mut subscriber = Subscriber::new(env.clone(), daemon.clone(), heartbeat);
        let _ = subscriber.connect();

        thread::spawn(move || {
            pump_changes(
                &stop,
                COALESCE_WINDOW,
                |budget| subscriber.poll(budget).is_some(),
                || tx.send(Event::DataChanged).is_ok(),
            )
        })
    }

    fn spawn_tick_thread(tx: Sender<Event>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(250));
                if tx.send(Event::Tick).is_err() {
                    break;
                }
            }
        })
    }
}

impl Drop for EventSource {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The change thread's whole loop, over a source of changes and a sink for the
/// events they produce.
///
/// Split out of [`EventSource::spawn_change_thread`] rather than left inline so
/// it can be driven by something other than a live daemon: `wait_for_change`
/// answers "did a change arrive within this budget?", `report` answers "is the
/// receiver still there?", and neither has to be real for the loop's own rules
/// to be exercised.
///
/// One change is reported per `window`, not one per change (SH-247): the first
/// change of a burst opens a window instead of reporting, everything arriving
/// inside it is read off the feed and folded into the same reload, and the
/// reload follows when the window closes. See [`COALESCE_WINDOW`] for why the
/// window is trailing-edge and why it is measured from the first change rather
/// than restarted by each one.
///
/// **Absorbing is not dropping.** Every path out of the window reports, the
/// shutdown path included — a change read off the feed and then not reported
/// is a write nothing will ever redraw for.
fn pump_changes(
    stop: &AtomicBool,
    window: Duration,
    mut wait_for_change: impl FnMut(Duration) -> bool,
    mut report: impl FnMut() -> bool,
) {
    while !stop.load(Ordering::Relaxed) {
        if !wait_for_change(POLL_BUDGET) {
            continue;
        }

        let deadline = Instant::now() + window;
        // `stop` is checked here as well as at the top so a shutdown waits out
        // at most one poll, not a poll and a whole window on top of it.
        while !stop.load(Ordering::Relaxed) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            // The change is what the window is for; its identity is not. Every
            // change means the same thing to the main loop — reload — so one
            // reload answers all of them.
            wait_for_change(remaining);
        }

        if !report() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{SqliteStore, Store as _, WriteOps as _};

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-tui-event-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    /// A [`DaemonInfo`] naming `port` and `token`. `/api/events` carries no
    /// version handshake, so only `port` and `token` are exercised by
    /// anything reachable from these tests. `token` must be the real one
    /// `serve()` minted (SH-187: every route requires it now, `/api/events`
    /// included) rather than a placeholder, or `spawn_change_thread`'s
    /// connection gets a 401.
    fn daemon_at(port: u16, token: &str) -> DaemonInfo {
        DaemonInfo {
            pid: std::process::id(),
            port,
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: 1,
            exe: std::env::current_exe().expect("this test binary"),
            exe_mtime: 0,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            token: token.to_string(),
            store_path: std::path::PathBuf::new(),
            tailnet: None,
            cookie_name: "storyhook_test".to_string(),
        }
    }

    /// Starts an in-process daemon over a fresh store and returns its port,
    /// environment, and a second store handle for tests to write through —
    /// standing in for "another checkout, a `story` command in a second
    /// terminal". The scratch directory is returned too, and must be held for
    /// the test's whole duration: the server runs on a detached thread that
    /// keeps reading and writing under it.
    ///
    /// Not `storyhook_test_support::serve`: that helper's signature takes
    /// this crate's own `SqliteStore`/`Environment`, and a unit test compiled
    /// as part of *this* crate's `--lib` test target is a second, distinct
    /// instance of it from the type system's point of view, so the types do
    /// not unify. `bind_and_serve` is what that helper wraps; called
    /// directly here, everything stays the same crate instance. The daemon
    /// this starts includes its own `poll_change_token` thread — the same
    /// `PRAGMA data_version` poll `spawn_change_thread` used to run itself —
    /// so a write through the second handle is detected and published onto
    /// the bus exactly as it would be in production, not simulated.
    fn serve() -> (u16, String, Environment, SqliteStore, tempfile::TempDir) {
        use std::sync::mpsc;

        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = SqliteStore::open(env.store_path()).expect("opening the store");
        store.migrate().expect("migrating");
        let writer = SqliteStore::open(env.store_path()).expect("a second handle");

        let (tx, rx) = mpsc::channel();
        let serving_env = env.clone();
        std::thread::spawn(move || {
            let store = store;
            let _ = crate::daemon::serve::bind_and_serve(
                &store,
                &serving_env,
                0,
                move |bound, token| {
                    let _ = tx.send((bound.port(), token));
                },
            );
        });
        let (port, token) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the test daemon never became ready");
        (port, token, env, writer, dir)
    }

    /// The property the whole thread exists for: a write made by something
    /// else wakes the interface.
    ///
    /// **This test is also the regression test for SH-140, and what it pins
    /// is its own determinism** — carried onto a different mechanism after
    /// SH-150. The write below is ordered strictly after
    /// `spawn_change_thread` returns, and nothing else synchronises the two,
    /// so the test only passes reliably if the subscription genuinely exists
    /// by the time this function returns. `Subscriber::connect` blocks for
    /// the daemon's own `: connected` frame for exactly this reason — the
    /// SSE-based equivalent of the store-polling version's "read the
    /// baseline before returning" fix.
    #[test]
    fn a_write_from_elsewhere_reports_a_change() {
        let (port, token, env, writer, _dir) = serve();
        let daemon = daemon_at(port, &token);

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _handle = EventSource::spawn_change_thread(&env, &daemon, tx, Arc::clone(&stop));

        writer
            .write(|tx| {
                tx.create_project(&crate::store::NewProject {
                    uuid: "tui-event-uuid".into(),
                    slug: "tui-event".into(),
                    name: "tui-event".into(),
                    prefix: "SH".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                })?;
                Ok(())
            })
            .expect("writing from another connection");

        let event = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a change must be reported");
        assert!(matches!(event, Event::DataChanged));
        stop.store(true, Ordering::Relaxed);
    }

    /// The other side of the boundary: a write that had already landed when
    /// the daemon started is history, not news.
    ///
    /// This exists to constrain the fix above, not the original defect.
    /// Reporting *every* connect as a change would pass
    /// `a_write_from_elsewhere_reports_a_change` too, and would also reload
    /// the board a moment after every TUI launch, on a store nobody has
    /// touched since. The daemon's own `poll_change_token` establishes its
    /// baseline at start-up (`src/daemon/serve.rs`), before this write is
    /// made here, which is what makes the distinction hold.
    #[test]
    fn a_write_that_landed_before_the_daemon_started_is_not_replayed() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = SqliteStore::open(env.store_path()).expect("opening the store");
        store.migrate().expect("migrating");
        store
            .write(|tx| {
                tx.create_project(&crate::store::NewProject {
                    uuid: "already-there-uuid".into(),
                    slug: "already-there".into(),
                    name: "already-there".into(),
                    prefix: "SH".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                })?;
                Ok(())
            })
            .expect("writing before the daemon starts");

        let (ready_tx, ready_rx) = mpsc::channel();
        let serving_env = env.clone();
        std::thread::spawn(move || {
            let _ = crate::daemon::serve::bind_and_serve(
                &store,
                &serving_env,
                0,
                move |bound, token| {
                    let _ = ready_tx.send((bound.port(), token));
                },
            );
        });
        let (port, token) = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the test daemon never became ready");
        let daemon = daemon_at(port, &token);

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _handle = EventSource::spawn_change_thread(&env, &daemon, tx, Arc::clone(&stop));

        assert!(
            rx.recv_timeout(POLL_BUDGET * 3).is_err(),
            "the write predates the daemon's own baseline, so it must not be \
             reported as a change"
        );
        stop.store(true, Ordering::Relaxed);
    }

    /// A quiet store must stay quiet: an interface told to reload every
    /// quarter second is one that loses your cursor position every quarter
    /// second.
    #[test]
    fn a_store_nobody_writes_to_reports_nothing() {
        let (port, token, env, _writer, _dir) = serve();
        let daemon = daemon_at(port, &token);

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _handle = EventSource::spawn_change_thread(&env, &daemon, tx, Arc::clone(&stop));

        assert!(
            rx.recv_timeout(Duration::from_millis(750)).is_err(),
            "nothing wrote to the store, so nothing should have been reported"
        );
        stop.store(true, Ordering::Relaxed);
    }

    /// The window the `pump_changes` tests below drive the loop with. Short
    /// enough to keep them quick; wide enough that the changes they script —
    /// a counter decrement each — land inside it with orders of magnitude to
    /// spare, so none of them races the window it is meant to sit inside.
    const TEST_WINDOW: Duration = Duration::from_millis(120);

    /// "Nothing arrived", answered the way [`Subscriber::poll`] answers it:
    /// by waiting out the budget it was given. Returning instantly instead
    /// would let the absorption loop spin through the window rather than wait
    /// in it, which is a different loop from the one production runs.
    fn quiet(budget: Duration) -> bool {
        thread::sleep(budget);
        false
    }

    /// SH-247: N changes arriving together cost one reload, not N.
    ///
    /// The feed is still drained of all five — absorbing is not dropping, and
    /// a change left unread would come back as the *next* burst.
    #[test]
    fn a_burst_of_changes_costs_a_single_reload() {
        let stop = AtomicBool::new(false);
        let mut pending = 5;
        let mut reports = 0;

        pump_changes(
            &stop,
            TEST_WINDOW,
            |budget| {
                if pending > 0 {
                    pending -= 1;
                    return true;
                }
                // The burst is spent: let the loop finish this window, report
                // once, and find `stop` set when it comes back round.
                stop.store(true, Ordering::Relaxed);
                quiet(budget)
            },
            || {
                reports += 1;
                true
            },
        );

        assert_eq!(pending, 0, "every change must be taken off the feed");
        assert_eq!(
            reports, 1,
            "five changes inside one window are one reload, not five"
        );
    }

    /// The trailing edge, stated as the two things it promises: a lone change
    /// is still reported, and it is reported *after* the window rather than on
    /// arrival.
    ///
    /// The delay is measured from the change's own arrival rather than from
    /// the top of the test, because only the former can tell the two edges
    /// apart: this harness answers "quiet" by sleeping, so a total elapsed
    /// time longer than a window says nothing about when the reload fired. A
    /// leading-edge throttle reports on arrival and suppresses what follows —
    /// SH-216's defect exactly — and would report at ~0 here.
    #[test]
    fn a_lone_change_is_reported_once_the_window_closes() {
        use std::cell::Cell;

        let stop = AtomicBool::new(false);
        let arrived_at: Cell<Option<Instant>> = Cell::new(None);
        let delay: Cell<Option<Duration>> = Cell::new(None);
        let mut pending = 1;
        let mut reports = 0;

        pump_changes(
            &stop,
            TEST_WINDOW,
            |budget| {
                if pending > 0 {
                    pending -= 1;
                    arrived_at.set(Some(Instant::now()));
                    return true;
                }
                stop.store(true, Ordering::Relaxed);
                quiet(budget)
            },
            || {
                delay.set(arrived_at.get().map(|at| at.elapsed()));
                reports += 1;
                true
            },
        );

        assert_eq!(reports, 1, "a change nothing joined is still a reload");
        assert!(
            delay.get().is_some_and(|d| d >= TEST_WINDOW),
            "the reload must follow the window, not the change's arrival: {:?}",
            delay.get()
        );
    }

    /// A change that arrives after the window closed is a second burst, and
    /// gets its own reload.
    ///
    /// This constrains the fix rather than reproducing the defect: collapsing
    /// two causally separate changes into one reload would leave the second
    /// one's write unshown until something else happened to trigger a reload.
    #[test]
    fn a_change_after_the_window_gets_its_own_reload() {
        let stop = AtomicBool::new(false);
        let mut step = 0;
        let mut reports = 0;

        pump_changes(
            &stop,
            TEST_WINDOW,
            |budget| {
                step += 1;
                // Steps 2 and 4 wait out a whole window quietly, so the change
                // at step 3 cannot be inside the one step 1 opened.
                match step {
                    1 | 3 => true,
                    _ => quiet(budget),
                }
            },
            || {
                reports += 1;
                if reports == 2 {
                    stop.store(true, Ordering::Relaxed);
                }
                true
            },
        );

        assert_eq!(
            reports, 2,
            "two changes a window apart are two reloads; collapsing them hides the second write"
        );
    }

    /// A store being written to without pause must still reload on a cadence.
    ///
    /// This is why the window is measured from the first change rather than
    /// restarted by each one. A restarting window is the textbook debounce and
    /// is wrong here: under a stream that never goes quiet — an import, a
    /// scripted sweep — it would defer the reload indefinitely and leave the
    /// board showing data from before the stream began. Written to fail on a
    /// timeout rather than hang, so that regression reports itself.
    #[test]
    fn a_stream_that_never_goes_quiet_still_reloads() {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let pumping = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            pump_changes(
                &pumping,
                TEST_WINDOW,
                |_| {
                    // Always another change waiting, paced so the absorption
                    // loop waits rather than spins.
                    thread::sleep(Duration::from_millis(1));
                    true
                },
                || tx.send(()).is_ok(),
            );
        });

        for nth in 1..=3 {
            rx.recv_timeout(TEST_WINDOW * 20).unwrap_or_else(|_| {
                panic!("reload {nth} never came: the window must not restart on every change")
            });
        }

        stop.store(true, Ordering::Relaxed);
        handle.join().expect("the thread must end rather than hang");
    }

    /// A daemon this cannot reach at all is not a reason to take the
    /// interface down — the same promise `an_unopenable_store_...` pinned
    /// before SH-150 moved what the thread watches.
    #[test]
    fn an_unreachable_daemon_leaves_the_interface_working() {
        // A bound-but-nothing-behind-it port: connections are refused
        // immediately rather than hanging, so this test stays fast.
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("binding a throwaway listener");
        let port = listener.local_addr().expect("the bound address").port();
        drop(listener); // freed; nothing answers there now

        let env = Environment::at("/private/tmp");
        // Never validated: nothing is listening on `port` at all, so the
        // connection itself fails before any token would be checked.
        let daemon = daemon_at(port, "unreachable");
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = EventSource::spawn_change_thread(&env, &daemon, tx, Arc::clone(&stop));
        assert!(rx.recv_timeout(Duration::from_millis(250)).is_err());
        stop.store(true, Ordering::Relaxed);
        handle.join().expect("the thread must end rather than hang");
    }
}
