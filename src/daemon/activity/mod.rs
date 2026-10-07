//! Daily operational journals (SH-590). Only the serving daemon installs the
//! process-wide sink; library users and CLI clients cannot capture each other's
//! activity. File-backed observers never wait for a descendant to close a pipe.

pub(crate) mod context;
pub(crate) mod hygiene;
mod ignore;
mod observe;
mod verifier_agent;
mod view;
pub(crate) mod window;
mod window_requests;

pub use ignore::{IGNORE_FILE, JOURNAL_IGNORE};
pub(crate) use observe::OutputWatch;
pub use view::{read_logs, read_logs_from};
pub(crate) use window::command_source;

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use chrono::{DateTime, SecondsFormat, Utc};
use fs4::FileExt;
use serde::{Deserialize, Serialize};

use crate::env::Environment;

/// Where a registered checkout keeps its project journal, relative to the
/// checkout (SH-748).
pub(crate) const PROJECT_JOURNAL: &str = ".storyhook/logs";

/// The project journal directory of `checkout`.
pub(crate) fn project_journal(checkout: &Path) -> PathBuf {
    checkout.join(PROJECT_JOURNAL)
}

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
    /// A directory that must still exist for this journal to write at all.
    anchor: Option<PathBuf>,
}

impl Journal {
    /// Selects a destination; the first append creates its private,
    /// self-ignoring directory.
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            anchor: None,
        }
    }

    /// A journal that writes only while `anchor` exists and never recreates
    /// it: its own directory beneath the anchor is still created on demand.
    ///
    /// The daemon's own journal is anchored to its state directory. Every
    /// directory above that is created when the daemon starts, so finding one
    /// gone means somebody deleted the tree on purpose. A test does exactly
    /// that when it ends, and a record written afterwards, the daemon's own
    /// "daemon stopped" among them, used to bring the whole deleted home back.
    pub(crate) fn anchored(directory: PathBuf, anchor: PathBuf) -> Self {
        Self {
            directory,
            anchor: Some(anchor),
        }
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
        if let Some(anchor) = &self.anchor
            && !anchor.is_dir()
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{} no longer exists, and a journal does not recreate a deleted tree",
                    anchor.display()
                ),
            ));
        }
        // Before the day file is opened: a journal file never exists in a
        // directory git can see (SH-771).
        if let Some(anchor) = &self.anchor {
            ignore::prepare_child(&self.directory, anchor)?;
        } else {
            ignore::prepare(&self.directory)?;
        }
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

/// Every day file in `directory`, sorted: what a test that reads a whole
/// journal must iterate. The directory also holds its ignore file and a
/// reader's `.view.lock`, and neither is a journal (SH-771).
#[cfg(test)]
pub(crate) fn day_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut days = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            days.push(path);
        }
    }
    days.sort();
    Ok(days)
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
    ACTIVE.get().is_some() || context::current().is_some()
}

/// Emits metadata without ever changing a command's outcome.
pub(crate) fn emit(level: &str, source: &str, stream: &str, context: &str, message: &str) {
    let project = self::context::current();
    let context = project.as_ref().map_or_else(
        || context.to_owned(),
        |project| format!("{} {context}", project.label),
    );
    if let Some(project) = project
        && let Err(error) = Journal::new(project.directory).append(
            Utc::now(),
            level,
            source,
            stream,
            &context,
            message,
        )
    {
        report_failure(&error);
    }
    if let Some(journal) = ACTIVE.get()
        && let Err(error) = journal.append(Utc::now(), level, source, stream, &context, message)
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
    if let Some(project) = context::current() {
        if !command
            .get_envs()
            .any(|(key, _)| key == "STORYHOOK_ACTIVITY_LOG_DIR")
        {
            command.env("STORYHOOK_ACTIVITY_LOG_DIR", project.directory);
        }
        command.env("STORYHOOK_ACTIVITY_CONTEXT", project.label);
    } else if let Some(journal) = ACTIVE.get()
        && !command
            .get_envs()
            .any(|(key, _)| key == "STORYHOOK_ACTIVITY_LOG_DIR")
    {
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
    let _ = ACTIVE.set(Journal::anchored(
        env.daemon_state_dir().join("activity"),
        env.daemon_state_dir(),
    ));
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

    /// SH-771: the first record into a new directory finds the ignore file
    /// already there, and a record into an existing, unprotected directory
    /// (every checkout journaled before SH-771) puts it back.
    #[test]
    fn every_append_leaves_the_directory_ignoring_itself() {
        let root = storyhook_test_support::scratch_dir();
        let directory = root.path().join(".storyhook/logs");
        let at = Utc::now();
        Journal::new(directory.clone())
            .append(at, "INFO", "daemon", "event", "", "first")
            .unwrap();
        assert_eq!(
            std::fs::read(directory.join(IGNORE_FILE)).unwrap(),
            JOURNAL_IGNORE
        );
        std::fs::remove_file(directory.join(IGNORE_FILE)).unwrap();
        Journal::new(directory.clone())
            .append(at, "INFO", "daemon", "event", "", "second")
            .unwrap();
        assert_eq!(
            std::fs::read(directory.join(IGNORE_FILE)).unwrap(),
            JOURNAL_IGNORE
        );
        assert_eq!(
            std::fs::read_to_string(day_path(&directory, at))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    /// SH-771: a record the ignore file cannot protect is refused, not
    /// written where git would see it.
    #[test]
    fn an_append_that_cannot_protect_its_directory_writes_no_journal_file() {
        let root = storyhook_test_support::scratch_dir();
        let directory = root.path().join(".storyhook/logs");
        std::fs::create_dir_all(directory.join(IGNORE_FILE)).unwrap();
        let at = Utc::now();
        assert!(
            Journal::new(directory.clone())
                .append(at, "INFO", "daemon", "event", "", "refused")
                .is_err()
        );
        assert!(!day_path(&directory, at).exists());
    }

    /// An anchored journal writes beneath its anchor, creating its own
    /// directory on demand, and once the anchor is deleted it writes nothing
    /// and creates nothing: a daemon's late record cannot bring back the
    /// home a test deleted.
    #[test]
    fn an_anchored_journal_never_recreates_a_deleted_tree() {
        let root = storyhook_test_support::scratch_dir();
        let home = root.path().join("home");
        let anchor = home.join(".local/state/storyhook/daemons/key");
        let directory = anchor.join("activity");
        std::fs::create_dir_all(&anchor).unwrap();
        let journal = Journal::anchored(directory.clone(), anchor.clone());
        let at = Utc::now();
        journal
            .append(at, "INFO", "daemon", "event", "", "started")
            .expect("a journal writes while its anchor exists");
        assert!(day_path(&directory, at).is_file());

        std::fs::remove_dir_all(&home).unwrap();
        let refused = journal
            .append(at, "INFO", "daemon", "event", "", "daemon stopped")
            .expect_err("a journal whose anchor is gone refuses the record");
        assert_eq!(refused.kind(), io::ErrorKind::NotFound);
        assert!(
            !home.exists(),
            "a record written after the tree was deleted recreated {}",
            home.display()
        );
    }
}
