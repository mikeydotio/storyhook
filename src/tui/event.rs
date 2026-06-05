use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, MouseEvent};

use crate::storage::ProjectPaths;

/// Events flowing through the TUI main loop.
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    DataChanged,
    Tick,
}

/// Manages background threads that produce events for the main loop.
///
/// Three threads:
/// 1. Input thread: crossterm::event::poll(100ms) + read()
/// 2. File watcher: notify::recommended_watcher on .storyhook/open/stories/
/// 3. Tick: 250ms sleep loop
pub struct EventSource {
    stop: Arc<AtomicBool>,
    _input_handle: JoinHandle<()>,
    _watcher_handle: JoinHandle<()>,
    _tick_handle: JoinHandle<()>,
}

impl EventSource {
    /// Create the event source and return the receiver for the main loop.
    pub fn new(root: &Path) -> (Self, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        let input_handle = Self::spawn_input_thread(tx.clone(), Arc::clone(&stop));
        let watcher_handle = Self::spawn_watcher_thread(root, tx.clone(), Arc::clone(&stop));
        let tick_handle = Self::spawn_tick_thread(tx, Arc::clone(&stop));

        let source = Self {
            stop,
            _input_handle: input_handle,
            _watcher_handle: watcher_handle,
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

    fn spawn_watcher_thread(
        root: &Path,
        tx: Sender<Event>,
        stop: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        let watch_path = ProjectPaths::new(root).open_stories_dir();
        thread::spawn(move || {
            Self::run_file_watcher(watch_path, tx, stop);
        })
    }

    fn run_file_watcher(watch_path: PathBuf, tx: Sender<Event>, stop: Arc<AtomicBool>) {
        use notify::{RecursiveMode, Watcher};

        let tx_notify = tx.clone();
        let debounce_ms = 200u128;
        let last_event = Arc::new(std::sync::Mutex::new(
            Instant::now() - Duration::from_secs(1),
        ));

        let last_event_clone = Arc::clone(&last_event);
        let mut watcher =
            match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if res.is_ok() {
                    let mut last = last_event_clone.lock().unwrap();
                    let now = Instant::now();
                    if now.duration_since(*last).as_millis() >= debounce_ms {
                        *last = now;
                        let _ = tx_notify.send(Event::DataChanged);
                    }
                }
            }) {
                Ok(w) => w,
                Err(_) => return, // If watcher creation fails, silently give up
            };

        // Watch the stories directory if it exists
        if watch_path.exists() {
            let _ = watcher.watch(&watch_path, RecursiveMode::NonRecursive);
        }

        // Keep the watcher alive until stop is signaled
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(250));
        }
        drop(watcher);
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
