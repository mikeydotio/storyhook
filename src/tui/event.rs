use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossterm::event::{KeyEvent, MouseEvent};

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

/// How often the store is asked whether anything has changed.
///
/// One pragma, so the interval can be short without mattering. It is the same
/// signal, at the same cadence, that the daemon's own change feed is built on.
const CHANGE_POLL: Duration = Duration::from_millis(250);

/// Manages background threads that produce events for the main loop.
///
/// Three threads:
/// 1. Input: `crossterm::event::poll(100ms)` + `read()`
/// 2. Change: the store's change token, polled
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
    /// # Why this watches the store rather than the filesystem
    ///
    /// It used to run a `notify` watcher over `.storyhook/`, which is where the
    /// data was. The data is in one database now, and a filesystem watcher over
    /// a repository would report a build artifact and miss a story created from
    /// another checkout — the opposite of what it is for.
    ///
    /// # Why it polls rather than subscribing to the daemon's feed
    ///
    /// **A deliberate departure**, recorded because the plan said SSE. The TUI
    /// holds its own store handle rather than going through the daemon — since
    /// SH-114 it is the last thing in storyhook that does, and it works with no
    /// daemon running at all (SH-150). Subscribing to a daemon's feed
    /// would make a TUI that works today stop updating on a machine where the
    /// daemon is not up — and it would learn the same fact, one layer further
    /// away, over a connection that can drop. The change token is what the
    /// daemon's own safety net polls; this reads it directly.
    pub fn new(env: &Environment) -> (Self, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        let input_handle = Self::spawn_input_thread(tx.clone(), Arc::clone(&stop));
        let change_handle = Self::spawn_change_thread(env, tx.clone(), Arc::clone(&stop));
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

    /// Watches the store for writes made by anything else — another checkout, a
    /// `story` command in a second terminal, the dashboard.
    ///
    /// Opens a connection of its own rather than borrowing the TUI's: this
    /// thread outlives no scope the store is in, and one extra SQLite handle is
    /// cheaper than threading a lifetime through the whole interface. A store it
    /// cannot open means no live updates — the TUI still works, and `r` still
    /// reloads — so the failure is silent by design rather than by neglect.
    fn spawn_change_thread(
        env: &Environment,
        tx: Sender<Event>,
        stop: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        use crate::store::Store as _;

        // **Both the open and the baseline read happen here, on the caller's
        // thread, and that placement is the whole point.**
        //
        // They used to happen inside the spawned closure, which lost writes
        // (SH-140). `thread::spawn` returns before the closure runs, so a write
        // committed in the gap was already counted when the baseline was
        // finally taken — and `PRAGMA data_version` only ever reports what has
        // happened *since the last read on this connection*. The comparison
        // below then never differed again, so the change was not reported late,
        // it could not be reported at all. Taking the baseline before returning
        // makes the write strictly later than the baseline for every caller.
        //
        // The store is moved rather than reopened because the token is
        // per-connection: a baseline read on one connection says nothing about
        // another's, so the connection that reads the baseline has to be the
        // one that goes on polling.
        let opened = crate::store::SqliteStore::open(env.store_path()).ok();
        let baseline = opened.as_ref().and_then(|store| store.change_token().ok());

        thread::spawn(move || {
            let Some(store) = opened else {
                return;
            };
            let mut last = baseline;
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(CHANGE_POLL);
                let Ok(token) = store.change_token() else {
                    continue;
                };
                if last != Some(token) {
                    last = Some(token);
                    if tx.send(Event::DataChanged).is_err() {
                        break;
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store as _, WriteOps as _};

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-tui-event-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    /// The property the whole thread exists for: a write made by something else
    /// wakes the interface.
    ///
    /// **This test is also the regression test for SH-140, and what it pins is
    /// its own determinism.** The write below is ordered after
    /// `spawn_change_thread` returns and nothing else synchronises the two, so
    /// the test only passes reliably if the baseline was taken *before* that
    /// return. While the baseline was read inside the spawned closure, a write
    /// landing in the gap was folded into it and no change could ever be
    /// reported — the wait did not expire because five seconds was too short, it
    /// expired because the event was impossible. That is why this failed once
    /// under `--test-threads=4`, passed alone in 0.28s, and never reproduced.
    ///
    /// The red was demonstrated rather than argued: a 300ms sleep at the top of
    /// the closure made it fail every run, with the same panic and the same full
    /// five-second wait as the recorded failure, and the same sleep passes with
    /// the baseline moved. A delayed thread start can no longer lose the event.
    #[test]
    fn a_write_from_elsewhere_reports_a_change() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = crate::store::SqliteStore::open(env.store_path()).expect("opening the store");
        store.migrate().expect("migrating");

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _handle = EventSource::spawn_change_thread(&env, tx, Arc::clone(&stop));

        // A second handle stands in for the other process: `data_version` moves
        // for commits made on a *different* connection, which is exactly the
        // case this is for.
        let other = crate::store::SqliteStore::open(env.store_path()).expect("a second handle");
        other
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
            .recv_timeout(Duration::from_secs(5))
            .expect("a change must be reported");
        assert!(matches!(event, Event::DataChanged));
        stop.store(true, Ordering::Relaxed);
    }

    /// The other side of the boundary: a write that had already landed when the
    /// watcher started is history, not news.
    ///
    /// This exists to constrain the fix above, not the original defect. Moving
    /// the baseline earlier fixes SH-140; so does deleting the baseline and
    /// starting from `None`, and that "fix" would report a change on the very
    /// first poll of every TUI launch — a reload a quarter second after start-up
    /// that nobody asked for, on a store nobody has touched. The two changes are
    /// indistinguishable to `a_write_from_elsewhere_reports_a_change` and
    /// opposite here.
    #[test]
    fn a_write_that_landed_before_the_watcher_started_is_not_replayed() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = crate::store::SqliteStore::open(env.store_path()).expect("opening the store");
        store.migrate().expect("migrating");

        // Committed on another connection, exactly as the reported case is, but
        // *before* the watcher exists to have an opinion about it.
        let other = crate::store::SqliteStore::open(env.store_path()).expect("a second handle");
        other
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
            .expect("writing before the watcher starts");

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _handle = EventSource::spawn_change_thread(&env, tx, Arc::clone(&stop));

        assert!(
            rx.recv_timeout(CHANGE_POLL * 3).is_err(),
            "the write predates the watcher, so it is already in the baseline \
             and must not be reported as a change"
        );
        stop.store(true, Ordering::Relaxed);
    }

    /// A quiet store must stay quiet: an interface told to reload every quarter
    /// second is one that loses your cursor position every quarter second.
    #[test]
    fn a_store_nobody_writes_to_reports_nothing() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = crate::store::SqliteStore::open(env.store_path()).expect("opening the store");
        store.migrate().expect("migrating");

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _handle = EventSource::spawn_change_thread(&env, tx, Arc::clone(&stop));

        assert!(
            rx.recv_timeout(Duration::from_millis(750)).is_err(),
            "nothing wrote to the store, so nothing should have been reported"
        );
        stop.store(true, Ordering::Relaxed);
    }

    /// A store that cannot be opened is not a reason to take the interface down.
    #[test]
    fn an_unopenable_store_leaves_the_interface_working() {
        let env = Environment::at("/proc/definitely/not/a/directory");
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = EventSource::spawn_change_thread(&env, tx, Arc::clone(&stop));
        assert!(rx.recv_timeout(Duration::from_millis(250)).is_err());
        stop.store(true, Ordering::Relaxed);
        handle.join().expect("the thread must end rather than hang");
    }
}
