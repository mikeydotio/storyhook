//! What the daemon leaves behind when it does not exit cleanly, and what the
//! next daemon does about it (SH-287).
//!
//! # The primitive this builds on
//!
//! A clean shutdown always ends in [`crate::daemon::lifecycle::clear_info`],
//! which removes the portfile. So a portfile that survives to be read by
//! something else — the next daemon's own start, or `stop` finding nobody
//! holding the pidfile lock — can only have been left by a daemon that did
//! not exit in an orderly way. `stop`'s own comment says this already
//! (`lifecycle.rs`); this module is what acts on it instead of only
//! deleting the evidence.
//!
//! # Two kinds of "did not exit cleanly"
//!
//! Most unclean exits are not defects: a reboot, a logout, a hand `kill`.
//! Only [`CrashClassification::Panicked`] is proof of one — the daemon's own
//! panic hook ([`install_panic_hook`]) caught it on the way out and wrote
//! down what it caught. Everything else is [`CrashClassification::UncleanExit`]:
//! ledgered for a human to review, never filed as a bug, because there is
//! nothing here that says the *code* was at fault.
//!
//! # Never the raw portfile
//!
//! [`crate::daemon::lifecycle::DaemonInfo`] carries a live bearer token
//! (SH-153's whole reasoning). [`CrashedDaemon`] is the redacted subset that
//! is safe to ledger, and later, safe to put in a bug report a human reads.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::daemon::lifecycle::{self, CurrentRequest, DaemonInfo};
use crate::env::Environment;

/// How many preserved crash logs are kept, oldest pruned first — the same
/// figure and the same reasoning [`crate::daemon::backup::RETAIN`] uses: a
/// week of daily use, and a bound on a directory nothing else prunes.
pub const RETAIN: usize = 7;

/// Where in the source a caught panic originated, when the panic carried one
/// — every panic from `panic!`, `assert!`, or an `unwrap`/`expect` does.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PanicLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

/// One panic the daemon's hook caught before the process unwound out from
/// under it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PanicRecord {
    /// RFC 3339, when the hook ran.
    pub at: String,
    /// The panicking process's pid — diagnostic only, the same caveat
    /// [`DaemonInfo::pid`] carries.
    pub pid: u32,
    /// The `storyhook` version that panicked.
    pub version: String,
    /// The name of the thread that panicked, or its numeric id when it has no
    /// name (every dispatcher thread [`crate::daemon::serve`] spawns is
    /// unnamed today).
    pub thread: String,
    /// What [`std::panic::PanicHookInfo::payload`] rendered as a string —
    /// almost always the `&str` or `String` a `panic!`/`assert!` was given.
    pub message: String,
    /// Where in the source, if the panic carried a location — it always does
    /// except across an FFI boundary storyhook does not have.
    pub location: Option<PanicLocation>,
}

/// The dead daemon's identity, with the one field that must never be
/// ledgered — its bearer token — stripped. Never construct this from
/// anything but a residue [`DaemonInfo`] this process is about to discard.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrashedDaemon {
    pub pid: u32,
    pub version: String,
    pub started_at: String,
}

impl From<&DaemonInfo> for CrashedDaemon {
    fn from(info: &DaemonInfo) -> Self {
        Self {
            pid: info.pid,
            version: info.version.clone(),
            started_at: info.started_at.clone(),
        }
    }
}

/// What the evidence says caused a daemon to stop.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CrashClassification {
    /// A panic record survived to be read — proof of a defect.
    Panicked,
    /// The OS itself wrote a crash report naming this pid and this signal —
    /// proof of a defect the panic hook could not have caught, because it
    /// only runs for a Rust panic and this is a fatal *signal*:
    /// `SIGSEGV`/`SIGBUS`/`SIGILL`/`SIGFPE`/`SIGTRAP`/`SIGABRT` raised
    /// outside one, most often inside an FFI boundary.
    FatalSignal(i32),
    /// A portfile survived with nobody holding its pidfile lock, and neither
    /// a panic record nor a matching crash report explains it. Most commonly
    /// a reboot, a logout, or a hand `kill -9` — `SIGKILL` is delivered by
    /// the kernel directly and raises no Mach exception, so it is invisible
    /// to both. Not, by itself, evidence of anything wrong with the daemon.
    UncleanExit,
}

/// One crash this store's daemon has noticed, ledgered for review.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrashRecord {
    /// Sortable and filesystem-safe: `story doctor crashes clear <id>` and
    /// the preserved log's filename both name a crash by this.
    pub id: String,
    /// RFC 3339, when this daemon noticed the residue — not when the crash
    /// itself happened, which is knowable only from [`Self::panics`].
    pub detected_at: String,
    pub classification: CrashClassification,
    /// The dead daemon's redacted identity, when a residue portfile could be
    /// parsed at all.
    pub daemon: Option<CrashedDaemon>,
    /// Every panic the hook caught before this crash, oldest first — usually
    /// one, occasionally more when a panic in one thread outlives the process
    /// long enough for another to also panic.
    pub panics: Vec<PanicRecord>,
    /// What this daemon was serving at the moment it stopped answering, read
    /// before anything else could touch it.
    pub inflight: Vec<CurrentRequest>,
    /// Where the dead daemon's own log was preserved, if there was one to
    /// preserve — a first spawn has none, and a launchd-started daemon has no
    /// [`lifecycle::spawn_child`] rotating one into place at all. Under
    /// [`crate::env::Environment::crash_logs_dir`], never under
    /// [`crate::env::Environment::daemon_log_rotated`], which the *next*
    /// spawn is free to overwrite.
    pub log_path: Option<PathBuf>,
}

/// A crash's ledger id: sortable, filesystem-safe, and stable enough to
/// reference from `story doctor crashes clear <id>`.
fn crash_id(detected_at: &str, pid: u32) -> String {
    format!("{}-{pid}", detected_at.replace(':', "-"))
}

/// Reads whatever panics the dying daemon's hook recorded, oldest first.
/// Absent or unparsable reads as none — the same treatment
/// [`lifecycle::read_abandoned`] gives its own file.
#[must_use]
pub fn read_panics(env: &Environment) -> Vec<PanicRecord> {
    let Ok(raw) = std::fs::read_to_string(env.daemon_panics()) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Writes the panic file atomically, readable only by its owner — the same
/// discipline [`write_crashes`] and every other file in this module use.
/// Removes the file rather than writing an empty array, so "no panics" and
/// "the file itself does not exist" stay the same meaning.
fn write_panics(env: &Environment, panics: &[PanicRecord]) {
    let path = env.daemon_panics();
    if panics.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let Ok(document) = serde_json::to_string(panics) else {
        return;
    };
    atomic_write(&path, document.as_bytes());
}

/// Appends `record` to the panic file.
///
/// Called from inside a panic hook, so **must not itself panic** — every
/// fallible step here degrades to "the panic goes unrecorded" rather than
/// risking a second panic inside the handler, which the runtime cannot
/// unwind and can only abort.
fn record_panic(env: &Environment, record: PanicRecord) {
    let mut panics = read_panics(env);
    panics.push(record);
    write_panics(env, &panics);
}

/// Clears the panic file — called once its contents have been folded into a
/// [`CrashRecord`] by [`harvest`], so a second daemon start does not read the
/// same panic twice.
fn clear_panics(env: &Environment) {
    let _ = std::fs::remove_file(env.daemon_panics());
}

/// Reads the crash ledger, oldest first.
#[must_use]
pub fn read_crashes(env: &Environment) -> Vec<CrashRecord> {
    let Ok(raw) = std::fs::read_to_string(env.daemon_crashes()) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Writes the crash ledger atomically, mode 0600 — a crash record can quote a
/// panic message that itself quoted something sensitive, so this file gets
/// the same protection [`crate::env::Environment::daemon_log`] does.
fn write_crashes(env: &Environment, ledger: &[CrashRecord]) {
    let path = env.daemon_crashes();
    if ledger.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let Ok(document) = serde_json::to_string(ledger) else {
        return;
    };
    atomic_write(&path, document.as_bytes());
}

/// Appends one record to the crash ledger.
fn record_crash(env: &Environment, record: CrashRecord) {
    let mut ledger = read_crashes(env);
    ledger.push(record);
    write_crashes(env, &ledger);
}

/// Forgets one ledgered crash by id, or every crash when `id` is `None` —
/// `story doctor crashes clear <id>` and `--all` respectively. Returns
/// whether anything was actually removed.
pub fn clear_crash(env: &Environment, id: Option<&str>) -> bool {
    let ledger = read_crashes(env);
    let before = ledger.len();
    let kept: Vec<CrashRecord> = match id {
        Some(id) => ledger.into_iter().filter(|c| c.id != id).collect(),
        None => Vec::new(),
    };
    let changed = kept.len() != before;
    if changed {
        write_crashes(env, &kept);
    }
    changed
}

/// Writes `bytes` to `path` atomically (temp file, then `rename`) and mode
/// 0600 — the same shape [`lifecycle::write_abandoned`] uses, duplicated
/// rather than shared because that function is private to its module and the
/// two files must never be able to clobber each other's temp name.
///
/// Creates `path`'s parent directory first, unlike `write_abandoned` — that
/// function can assume [`Environment::daemon_state_dir`] already exists
/// because everything that calls it runs after a daemon has bound a port
/// there; [`harvest`] can run from [`lifecycle::stop`]'s not-live branch,
/// which has no such guarantee.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temp = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let Ok(mut file) = options.open(&temp) else {
        return;
    };
    if file.write_all(bytes).is_err() {
        let _ = std::fs::remove_file(&temp);
        return;
    }
    let _ = std::fs::rename(&temp, path);
}

/// Installs a panic hook that records a [`PanicRecord`] before chaining to
/// whatever hook was installed before it — so `daemon.log` still receives the
/// default panic output verbatim, unchanged from today.
///
/// Idempotent in effect but not in cost: call once, at daemon startup. A
/// second call would chain hooks rather than replace one, recording every
/// panic twice.
pub fn install_panic_hook(env: &Environment) {
    let env = env.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_panic(&env, panic_record(&env, info));
        previous(info);
    }));
}

/// Builds the [`PanicRecord`] a hook writes, from what
/// [`std::panic::PanicHookInfo`] and the ambient process carry.
fn panic_record(env: &Environment, info: &std::panic::PanicHookInfo<'_>) -> PanicRecord {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked with a non-string payload".to_string());
    let location = info.location().map(|loc| PanicLocation {
        file: loc.file().to_string(),
        line: loc.line(),
        column: loc.column(),
    });
    let thread = std::thread::current();
    PanicRecord {
        at: env.now(),
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        thread: thread
            .name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("{:?}", thread.id())),
        message,
        location,
    }
}

/// `STORYHOOK_TEST_PANIC=1` makes a running daemon panic on its own, once it
/// is fully up — the out-of-process lever an integration test uses to prove
/// [`install_panic_hook`] catches a *real* crash, not just one
/// [`std::panic::catch_unwind`] triggers directly in a unit test.
///
/// The same idiom `STORYHOOK_FAULT` already established for
/// [`crate::store::fault`]: an env var read only in a build carrying live
/// crash points, so it cannot exist in anything a user runs. Called from
/// [`crate::daemon::serve::serve`], after the portfile is published and after
/// startup recovery ([`crate::daemon::lifecycle::InFlight::harvest_stale`])
/// has run — panicking any earlier would leave nothing for the *next* daemon
/// to find a residue portfile for, which is the scenario this exists to test.
#[cfg(feature = "fault-injection")]
pub fn maybe_trigger_test_panic() {
    if std::env::var_os("STORYHOOK_TEST_PANIC").is_some() {
        panic!("STORYHOOK_TEST_PANIC: a deliberate panic for SH-287's crash-detection tests");
    }
}

/// Looks at whatever portfile is on disk right now and decides whether the
/// daemon that wrote it crashed.
///
/// Reaching this function at all already implies the writer is gone — both
/// call sites establish that structurally before calling it:
/// [`lifecycle::stop`]'s not-live branch only calls it once `is_live` has
/// answered `false`, and [`lifecycle::run`] calls it just after
/// [`lifecycle::claim_pidfile`] succeeds, which could not have happened had
/// anyone else still held the lock. So `harvest` itself asks no liveness
/// question — only "is there a residue portfile to explain."
///
/// Consumes [`Self::daemon_panics`] via [`clear_panics`]: whatever it held
/// becomes [`CrashRecord::panics`], and the file is cleared so a second call
/// never double-counts it. **Does not delete the portfile** — that stays the
/// caller's job, exactly as it is today (`stop` removes it explicitly;
/// `run` overwrites it with this daemon's own info a few lines later).
///
/// Returns `None` when there was no residue portfile to investigate — the
/// common case, an orderly prior stop having already cleared it.
pub fn harvest(env: &Environment) -> Option<CrashRecord> {
    let residue = lifecycle::read_info(env)?;
    let panics = read_panics(env);
    clear_panics(env);
    let inflight = lifecycle::read_inflight(env);
    let detected_at = env.now();
    let id = crash_id(&detected_at, residue.pid);
    let classification = classify(
        &panics,
        fatal_signal_report(env, residue.pid, &residue.started_at),
    );
    let log_path = preserve_log(env, &id);
    let record = CrashRecord {
        id,
        detected_at,
        classification,
        daemon: Some(CrashedDaemon::from(&residue)),
        panics,
        inflight,
        log_path,
    };
    record_crash(env, record.clone());
    Some(record)
}

/// The pure decision [`harvest`] hands its evidence to — no filesystem, no
/// clock, fully covered by unit tests that never touch a real crash report.
fn classify(panics: &[PanicRecord], fatal_signal: Option<i32>) -> CrashClassification {
    if !panics.is_empty() {
        CrashClassification::Panicked
    } else if let Some(signal) = fatal_signal {
        CrashClassification::FatalSignal(signal)
    } else {
        CrashClassification::UncleanExit
    }
}

/// Best-effort: a fatal signal the OS itself reported for `pid`, if it wrote
/// a crash report after `started_at` and this build knows how to look.
///
/// Absent entirely off macOS, and absent in a test build's isolated
/// environment even on one — [`Environment::home`] is redirected under
/// [`crate::env::is_test_build`]'s isolation, so this looks in a directory
/// that does not exist and finds nothing, which is the correct answer for a
/// test: it must never attribute a developer's real crash report to a
/// fixture daemon.
#[cfg(target_os = "macos")]
fn fatal_signal_report(env: &Environment, pid: u32, started_at: &str) -> Option<i32> {
    let after = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let dir = env.home().join("Library/Logs/DiagnosticReports");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|entry| {
        std::cmp::Reverse(entry.metadata().and_then(|meta| meta.modified()).ok())
    });
    for entry in entries.into_iter().take(50) {
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if modified < std::time::SystemTime::from(after) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if names_pid(&contents, pid)
            && let Some(signal) = extract_fatal_signal(&contents)
        {
            return Some(signal);
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn fatal_signal_report(_env: &Environment, _pid: u32, _started_at: &str) -> Option<i32> {
    None
}

/// Whether a macOS crash report's text names `pid` — the JSON-lines format
/// (macOS 12+) and the older text format spell the field differently, so both
/// are checked. A pure string search, unit-tested with fixture text rather
/// than a real report.
#[cfg(target_os = "macos")]
fn names_pid(contents: &str, pid: u32) -> bool {
    contents.contains(&format!("\"pid\" : {pid}"))
        || contents.contains(&format!("\"pid\":{pid}"))
        || contents.contains(&format!("Identifier:{pid}"))
        || contents.contains(&format!("PID: {pid}"))
}

/// The fatal signal a macOS crash report's text names, if any of the six that
/// raise a Mach exception appear in it. Checked in a fixed order so a report
/// naming more than one (some do, in an "Exception Codes" line alongside the
/// underlying `si_signo`) resolves the same way every time. A pure string
/// search, unit-tested with fixture text rather than a real report.
#[cfg(target_os = "macos")]
fn extract_fatal_signal(contents: &str) -> Option<i32> {
    const NAMED: &[(&str, i32)] = &[
        ("SIGSEGV", libc::SIGSEGV),
        ("SIGBUS", libc::SIGBUS),
        ("SIGILL", libc::SIGILL),
        ("SIGFPE", libc::SIGFPE),
        ("SIGTRAP", libc::SIGTRAP),
        ("SIGABRT", libc::SIGABRT),
    ];
    NAMED
        .iter()
        .find(|(name, _)| contents.contains(name))
        .map(|(_, signal)| *signal)
}

/// Moves the previous daemon's rotated log into [`Environment::crash_logs_dir`]
/// under `id`, mode 0600, then prunes to the newest [`RETAIN`] — the same
/// pattern [`crate::daemon::backup::run_if_due`] and its own `prune` use for
/// the same reason: a directory nothing else bounds is a directory that grows
/// forever.
///
/// Returns `None` without error when there was no rotated log to preserve — a
/// daemon's first-ever spawn, or one launchd started, which never rotates one
/// in ([`lifecycle::spawn_child`] is the only writer of
/// [`Environment::daemon_log_rotated`]).
fn preserve_log(env: &Environment, id: &str) -> Option<PathBuf> {
    let source = env.daemon_log_rotated();
    if !source.exists() {
        return None;
    }
    let dir = env.crash_logs_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let dest = dir.join(format!("{id}.log"));
    std::fs::rename(&source, &dest).ok()?;
    prune_logs(&dir);
    Some(dest)
}

/// Keeps the newest [`RETAIN`] preserved crash logs in `dir`, oldest first
/// out — and says what it dropped on `daemon.log`, rather than pruning in
/// silence: a silent cap reads as "every crash log was kept" when it was not.
///
/// Sorted by filename, not by mtime — [`crate::daemon::backup::snapshots`]'s
/// own reasoning applies unchanged: a crash log is named `<id>.log`, and
/// [`crash_id`] embeds an RFC 3339 timestamp with fixed-width, most-significant-
/// first fields, so lexicographic order on the name already **is**
/// chronological order. Filesystem mtimes are a second, independent clock
/// that can disagree with it (a restored backup, a clock adjustment) and a
/// test would otherwise have to fight to control.
fn prune_logs(dir: &std::path::Path) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
        .collect();
    if logs.len() <= RETAIN {
        return;
    }
    logs.sort();
    let drop_count = logs.len() - RETAIN;
    for path in &logs[..drop_count] {
        let _ = std::fs::remove_file(path);
    }
    eprintln!(
        "storyhook daemon: pruned {drop_count} crash log{} beyond the newest {RETAIN}",
        if drop_count == 1 { "" } else { "s" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::lifecycle::{info_for, write_info};
    use crate::daemon::serve::BoundAddress;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-crash-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    fn loopback_only(port: u16) -> BoundAddress {
        BoundAddress {
            loopback: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            tailnet: None,
        }
    }

    fn a_residue_portfile(env: &Environment) {
        let info = info_for(
            &loopback_only(4321),
            "secret-token".to_string(),
            "2026-01-01T00:00:00Z",
            env.store_path(),
        )
        .expect("building a portfile");
        write_info(env, &info).expect("writing it");
    }

    #[test]
    fn no_residue_portfile_harvests_nothing() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert!(harvest(&env).is_none());
        assert!(read_crashes(&env).is_empty());
    }

    #[test]
    fn a_bare_residue_portfile_ledgers_as_an_unclean_exit() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        a_residue_portfile(&env);

        let record = harvest(&env).expect("a residue portfile to harvest");
        assert_eq!(record.classification, CrashClassification::UncleanExit);
        assert!(record.panics.is_empty());
        // `info_for` always stamps the pid of whatever process built the
        // portfile — this test process, not a value `loopback_only` chose (it
        // only sets the port).
        assert_eq!(
            record.daemon.as_ref().map(|d| d.pid),
            Some(std::process::id())
        );

        let ledgered = read_crashes(&env);
        assert_eq!(ledgered, vec![record]);
    }

    #[test]
    fn a_recorded_panic_ledgers_as_panicked_and_is_consumed() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        a_residue_portfile(&env);
        record_panic(
            &env,
            PanicRecord {
                at: "2026-01-01T00:00:01Z".to_string(),
                pid: 4321,
                version: "2.1.1".to_string(),
                thread: "dispatcher-0".to_string(),
                message: "index out of bounds".to_string(),
                location: Some(PanicLocation {
                    file: "src/daemon/serve.rs".to_string(),
                    line: 42,
                    column: 5,
                }),
            },
        );

        let record = harvest(&env).expect("a residue portfile to harvest");
        assert_eq!(record.classification, CrashClassification::Panicked);
        assert_eq!(record.panics.len(), 1);
        assert_eq!(record.panics[0].message, "index out of bounds");

        // Consumed: a second harvest of the same (still-present) residue
        // portfile must not see the panic again.
        a_residue_portfile(&env);
        let second = harvest(&env).expect("the residue is still there");
        assert_eq!(second.classification, CrashClassification::UncleanExit);
        assert!(second.panics.is_empty());
    }

    #[test]
    fn the_daemon_info_embedded_in_a_crash_record_never_carries_the_bearer_token() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        a_residue_portfile(&env);

        let record = harvest(&env).expect("a residue portfile to harvest");
        let serialized = serde_json::to_string(&record).expect("serializing the record");
        assert!(
            !serialized.contains("secret-token"),
            "a crash record must never carry the dead daemon's bearer token: {serialized}"
        );
    }

    /// A minimal, otherwise-uninteresting record for the ledger tests below,
    /// which only care about `id`.
    fn bare_crash(id: &str, at: &str) -> CrashRecord {
        CrashRecord {
            id: id.to_string(),
            detected_at: at.to_string(),
            classification: CrashClassification::UncleanExit,
            daemon: None,
            panics: Vec::new(),
            inflight: Vec::new(),
            log_path: None,
        }
    }

    #[test]
    fn clearing_one_crash_by_id_leaves_the_others() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        record_crash(&env, bare_crash("keep-me", "2026-01-01T00:00:00Z"));
        record_crash(&env, bare_crash("forget-me", "2026-01-01T00:00:01Z"));

        assert!(clear_crash(&env, Some("forget-me")));
        let remaining = read_crashes(&env);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "keep-me");

        assert!(!clear_crash(&env, Some("already-gone")));
    }

    #[test]
    fn clearing_all_crashes_empties_the_ledger_and_removes_the_file() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        record_crash(&env, bare_crash("one", "2026-01-01T00:00:00Z"));
        assert!(clear_crash(&env, None));
        assert!(read_crashes(&env).is_empty());
        assert!(!env.daemon_crashes().exists());
    }

    #[test]
    fn classify_prefers_a_panic_over_a_fatal_signal() {
        let panics = vec![PanicRecord {
            at: "2026-01-01T00:00:00Z".to_string(),
            pid: 1,
            version: "2.1.1".to_string(),
            thread: "main".to_string(),
            message: "oops".to_string(),
            location: None,
        }];
        assert_eq!(
            classify(&panics, Some(libc::SIGSEGV)),
            CrashClassification::Panicked
        );
    }

    #[test]
    fn classify_falls_back_to_a_fatal_signal_with_no_panic() {
        assert_eq!(
            classify(&[], Some(libc::SIGSEGV)),
            CrashClassification::FatalSignal(libc::SIGSEGV)
        );
    }

    #[test]
    fn classify_is_an_unclean_exit_with_neither() {
        assert_eq!(classify(&[], None), CrashClassification::UncleanExit);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn names_pid_matches_both_report_formats() {
        assert!(names_pid(r#"{"pid" : 4321, "other": 1}"#, 4321));
        assert!(names_pid(r#"{"pid":4321}"#, 4321));
        assert!(names_pid("Identifier:4321\nsome other line", 4321));
        assert!(!names_pid(r#"{"pid" : 4322}"#, 4321));
        assert!(!names_pid("nothing relevant here", 4321));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn extract_fatal_signal_recognizes_every_named_signal() {
        assert_eq!(
            extract_fatal_signal("Exception Type: EXC_BAD_ACCESS (SIGSEGV)"),
            Some(libc::SIGSEGV)
        );
        assert_eq!(
            extract_fatal_signal("Termination Reason: SIGABRT"),
            Some(libc::SIGABRT)
        );
        assert_eq!(extract_fatal_signal("no signal named here"), None);
    }

    #[test]
    fn preserve_log_moves_the_rotated_log_and_prunes_the_oldest() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the state dir");

        // Seed RETAIN pre-existing preserved logs, each named so its
        // lexicographic order matches its chronological one — the same
        // property a real crash id has, so pruning has an unambiguous oldest
        // to drop without needing to control any file's mtime.
        let logs_dir = env.crash_logs_dir();
        std::fs::create_dir_all(&logs_dir).expect("the crash logs dir");
        for i in 0..RETAIN {
            let path = logs_dir.join(format!("2020-01-0{}T00-00-00Z-1.log", i + 1));
            std::fs::write(&path, "old").expect("seeding an old log");
        }

        std::fs::write(env.daemon_log_rotated(), "the dead daemon's stderr")
            .expect("writing a rotated log to preserve");

        let preserved = preserve_log(&env, "new-crash").expect("a rotated log existed");
        assert_eq!(preserved, logs_dir.join("new-crash.log"));
        assert_eq!(
            std::fs::read_to_string(&preserved).expect("reading the preserved log"),
            "the dead daemon's stderr"
        );
        assert!(
            !env.daemon_log_rotated().exists(),
            "the rotated log must be moved, not copied"
        );

        let remaining: Vec<_> = std::fs::read_dir(&logs_dir)
            .expect("reading the crash logs dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            remaining.len(),
            RETAIN,
            "pruning must keep exactly RETAIN logs: {remaining:?}"
        );
        assert!(
            !remaining.contains(&"2020-01-01T00-00-00Z-1.log".to_string()),
            "the lexicographically oldest seeded log must be the one pruned: {remaining:?}"
        );
        assert!(remaining.contains(&"new-crash.log".to_string()));
    }

    #[test]
    fn preserve_log_is_none_when_there_is_nothing_rotated() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert!(preserve_log(&env, "no-residue").is_none());
    }

    #[test]
    fn harvest_preserves_the_rotated_log_and_names_it_in_the_record() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let info = info_for(
            &loopback_only(4321),
            "t".to_string(),
            "2026-01-01T00:00:00Z",
            env.store_path(),
        )
        .expect("building a portfile");
        write_info(&env, &info).expect("writing it");
        std::fs::write(env.daemon_log_rotated(), "the dead daemon's own stderr")
            .expect("writing a rotated log");

        let record = harvest(&env).expect("a residue portfile to harvest");
        let log_path = record.log_path.expect("the rotated log must be preserved");
        assert_eq!(
            std::fs::read_to_string(&log_path).expect("reading the preserved log"),
            "the dead daemon's own stderr"
        );
    }
}
