//! Daily operational journals (SH-590). Only the serving daemon installs the
//! process-wide sink; library users and CLI clients cannot capture each other's
//! activity. File-backed observers never wait for a descendant to close a pipe.

mod observe;
mod view;
mod window;

pub(crate) use observe::OutputWatch;
pub use view::read_logs;
pub(crate) use window::command_source;

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use chrono::{DateTime, SecondsFormat, Utc};
use fs4::FileExt;
use serde::{Deserialize, Serialize};

use crate::env::Environment;

static ACTIVE: OnceLock<Journal> = OnceLock::new();
static DIAGNOSTICS: Mutex<Option<OutputWatch>> = Mutex::new(None);
static STOPPED: AtomicBool = AtomicBool::new(false);
static REPORTED_FAILURE: AtomicBool = AtomicBool::new(false);

/// The shared wire format also written by `scripts/activity-run.py`.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    at: String,
    level: String,
    source: String,
    stream: String,
    pid: u32,
    context: String,
    message: String,
}

/// A store's append-only daily journal. Each record takes an OS file lock so
/// Rust daemon threads and verifier script processes share one framing rule.
pub(crate) struct Journal {
    directory: PathBuf,
}

impl Journal {
    /// Selects a destination; the first append creates its private directory.
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn append(
        &self,
        at: DateTime<Utc>,
        level: &str,
        source: &str,
        stream: &str,
        context: &str,
        message: &str,
    ) -> io::Result<()> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.directory)?;
        let record = Record {
            at: at.to_rfc3339_opts(SecondsFormat::Millis, true),
            level: clean(level),
            source: clean(source),
            stream: clean(stream),
            pid: std::process::id(),
            context: clean(context),
            message: clean(message),
        };
        let mut encoded = serde_json::to_vec(&record)?;
        encoded.push(b'\n');
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(day_path(&self.directory, at))?;
        file.lock_exclusive()?;
        file.write_all(&encoded)
    }
}

fn day_path(directory: &Path, at: DateTime<Utc>) -> PathBuf {
    directory.join(format!("{}.jsonl", at.format("%Y-%m-%d")))
}

fn clean(text: &str) -> String {
    crate::daemon::crash::redact(text)
        .chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

/// Whether this process is a daemon with an installed activity sink.
pub(crate) fn enabled() -> bool {
    ACTIVE.get().is_some()
}

/// Emits metadata without ever changing a command's outcome.
pub(crate) fn emit(level: &str, source: &str, stream: &str, context: &str, message: &str) {
    if let Some(journal) = ACTIVE.get()
        && let Err(error) = journal.append(Utc::now(), level, source, stream, context, message)
    {
        report_failure(&error);
    }
}

fn report_failure(error: &io::Error) {
    // The daemon stderr observer also reaches this path. One report prevents
    // a failed disk from producing an endless failure-about-failure loop.
    if !REPORTED_FAILURE.swap(true, Ordering::Relaxed) {
        eprintln!("warning: activity journal unavailable: {error}");
    }
}

/// Passes only the journal destination to owned scripts, never a store handle.
pub(crate) fn configure(command: &mut std::process::Command) {
    if let Some(journal) = ACTIVE.get() {
        command.env("STORYHOOK_ACTIVITY_LOG_DIR", &journal.directory);
    }
}

/// Flushes the diagnostic observer and records a normal daemon return.
pub(crate) struct ActivityGuard;

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        stop();
    }
}

/// Flushes output before an orderly process exit, which does not run Drop.
pub(crate) fn stop() {
    if STOPPED.swap(true, Ordering::AcqRel) {
        return;
    }
    let observer = DIAGNOSTICS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    drop(observer);
    emit("INFO", "daemon", "event", "", "daemon stopped");
}

/// Installs one sink after the daemon owns its lifetime lock. The tmux work
/// runs separately so a terminal server can never delay daemon readiness.
pub(crate) fn start(env: &Environment) -> ActivityGuard {
    let _ = ACTIVE.set(Journal::new(env.daemon_state_dir().join("activity")));
    emit(
        "INFO",
        "daemon",
        "event",
        "",
        &format!("daemon started {}", crate::version::full()),
    );
    let diagnostics = match File::open(env.daemon_log())
        .and_then(|file| Ok((file.metadata()?.len(), file)))
    {
        Ok((offset, file)) => OutputWatch::start("daemon", "", vec![("stderr", file, offset)]),
        Err(error) => {
            emit(
                "WARN",
                "logger",
                "event",
                "",
                &format!(
                    "cannot observe daemon stderr at {}: {error}; foreground diagnostics remain on the terminal",
                    env.daemon_log().display()
                ),
            );
            None
        }
    };
    *DIAGNOSTICS.lock().unwrap_or_else(PoisonError::into_inner) = diagnostics;
    if env.verifier_mirror_enabled() {
        #[cfg(test)]
        isolation_tests::WINDOW_STARTS.fetch_add(1, Ordering::SeqCst);
        let env = env.clone();
        std::thread::spawn(move || window::open(&env));
    }
    ActivityGuard
}

#[cfg(test)]
#[path = "tests.rs"]
mod isolation_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_rotation_restart_and_concurrent_records_keep_complete_private_json() {
        let root = storyhook_test_support::scratch_dir();
        let directory = root.path().join("logs with spaces");
        let first = DateTime::parse_from_rfc3339("2026-09-07T23:59:59Z")
            .unwrap()
            .to_utc();
        let next = first + chrono::Duration::seconds(1);
        let log = Journal::new(directory.clone());
        log.append(first, "INFO", "daemon", "event", "SH-1", "before")
            .unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let log = &log;
                scope.spawn(move || {
                    for _ in 0..30 {
                        log.append(
                            next,
                            "WARN",
                            "script.sh",
                            "stderr",
                            "SH-2",
                            "a\nb\r\u{1b}[31m ghp_secret Authorization: secret",
                        )
                        .unwrap();
                    }
                });
            }
        });
        Journal::new(directory.clone())
            .append(next, "INFO", "daemon", "event", "", "restart")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(day_path(&directory, first))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let text = std::fs::read_to_string(day_path(&directory, next)).unwrap();
        assert_eq!(text.lines().count(), 121);
        for line in text.lines() {
            serde_json::from_str::<Record>(line).unwrap();
        }
        assert!(!text.contains("ghp_secret"));
        assert!(!text.contains("Authorization: secret"));
        assert!(!text.contains('\u{1b}'));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(day_path(&directory, next))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
