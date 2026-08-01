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
    /// holds its own store handle: it is a `--local` client by construction, and
    /// it works with no daemon running at all. Subscribing to a daemon's feed
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
        let path = env.store_path().to_path_buf();
        thread::spawn(move || {
            use crate::store::Store as _;
            let Ok(store) = crate::store::SqliteStore::open(path) else {
                return;
            };
            let mut last = store.change_token().ok();
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
