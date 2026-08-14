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
use sha2::{Digest, Sha256};

use crate::daemon::lifecycle::{self, CurrentRequest, DaemonInfo};
use crate::domain::provenance::Provenance;
use crate::env::Environment;
use crate::service::Ctx;
use crate::service::query::QueryService;
use crate::service::story::{NewStoryInput, StoryService};
use crate::store::{ProjectId, ReadOps, Store};

/// How many preserved crash logs are kept, oldest pruned first — the same
/// figure and the same reasoning [`crate::daemon::backup::RETAIN`] uses: a
/// week of daily use, and a bound on a directory nothing else prunes.
pub const RETAIN: usize = 7;

/// Every crash-filed story carries this label — what `story doctor crashes`
/// and the github-sync push filter both key on.
pub const CRASH_LABEL: &str = "crash";

/// Prefix for a crash's fingerprint label (`crash:<8 hex chars>`) — the one
/// slot [`crate::service::query::QueryService::search`] can find it in, since
/// search covers titles, comments and labels but not descriptions.
const FINGERPRINT_LABEL_PREFIX: &str = "crash:";

/// Marks a story this daemon wrote, not a human.
const AUTO_FILED_LABEL: &str = "auto-filed";

/// The uuid this repository's own committed `.storyhook.toml` declares —
/// storyhook's own project, and the only place SH-287 files a crash it
/// caused in itself. Specific to this checkout by design (a decision
/// SH-287's plan makes explicit): a crash story belongs beside every other
/// storyhook defect, not in whatever project happened to be open when the
/// daemon died.
const SELF_PROJECT_UUID: &str = "291ea25f-3363-4b5d-9051-66636c1066f9";

/// How many new stories one daemon start will file, at most. A crashloop
/// ledgers the rest as [`FiledOutcome::Withheld`] rather than minting one
/// story per iteration — this bounds only *new* stories; folding a repeat
/// into an existing one via [`FiledOutcome::Deduped`] is cheap and uncapped.
const MAX_FILED_PER_START: usize = 3;

/// The largest log excerpt a filed story's description carries. Filing runs
/// in-process, so [`crate::api::http::MAX_BODY_BYTES`] (the wire cap on
/// `/api/v1/invoke`) does not apply here — this bound is the store's own, to
/// keep an unbounded blob out of a description column nothing else limits.
const MAX_LOG_EXCERPT_BYTES: usize = 32 * 1024;

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

impl CrashClassification {
    /// Whether this classification is proof of a defect worth filing a bug
    /// for. An `UncleanExit` alone never is — SH-287's own decision, since
    /// most are a reboot or a hand `kill`, not a defect.
    #[must_use]
    pub fn is_defect_evidence(&self) -> bool {
        !matches!(self, Self::UncleanExit)
    }
}

/// What became of a crash's bug report.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FiledOutcome {
    /// Not yet decided — [`harvest`]'s own default, resolved the next time
    /// [`file_pending`] runs.
    Pending,
    /// A new story was created; the id is its own.
    Filed(String),
    /// The same fingerprint already produced a story; a "seen again" comment
    /// was folded into it instead of minting a duplicate.
    Deduped(String),
    /// Deliberately not filed, and why — an `UncleanExit`, no project
    /// registered, the per-start cap, or a store error worth retrying next
    /// start. Never silent: `story doctor crashes` reads this.
    Withheld(String),
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
    /// What became of this crash's bug report — [`FiledOutcome::Pending`]
    /// until [`file_pending`] decides otherwise.
    pub filed: FiledOutcome,
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
///
/// The variable's *value* is folded into the panic message when it is
/// anything other than `1` — `STORYHOOK_TEST_PANIC=distinct-marker` — so a
/// test needing several non-deduplicating crashes (the per-start filing cap,
/// say) can tell them apart without a second lever.
#[cfg(feature = "fault-injection")]
pub fn maybe_trigger_test_panic() {
    let Some(marker) = std::env::var_os("STORYHOOK_TEST_PANIC") else {
        return;
    };
    let marker = marker.to_string_lossy();
    if marker.as_ref() == "1" {
        panic!("STORYHOOK_TEST_PANIC: a deliberate panic for SH-287's crash-detection tests");
    }
    panic!("STORYHOOK_TEST_PANIC({marker}): a deliberate panic for SH-287's crash-detection tests");
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
        filed: FiledOutcome::Pending,
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

/// Whether this build may write a crash story to a real tracker.
///
/// [`crate::env::is_test_build`] already answers "may this build write to a
/// real tracker?" for the store; the same answer applies here for the same
/// reason. `STORYHOOK_CRASH_FILE=1` overrides it, the same shape
/// `STORYHOOK_FAULT` uses, so the integration suite can prove filing works
/// end to end against its own isolated store without a real user ever being
/// able to trigger it.
fn should_file() -> bool {
    !crate::env::is_test_build() || std::env::var_os("STORYHOOK_CRASH_FILE").is_some()
}

/// Files a bug story for every ledgered crash that is defect evidence and
/// still [`FiledOutcome::Pending`], folds a "seen again" comment into
/// whichever story a repeat fingerprint already produced, and records every
/// refusal with its reason.
///
/// Called once per daemon start, on a background thread after `ready()` —
/// see the call site in [`crate::daemon::serve::serve`] for why a store write
/// must never delay startup. Best-effort throughout: a store error part-way
/// through leaves the remaining records `Pending` for the *next* daemon start
/// to retry, rather than losing them.
pub fn file_pending<S: Store>(store: &S, env: &Environment) {
    if !should_file() {
        return;
    }
    let mut ledger = read_crashes(env);
    if !ledger
        .iter()
        .any(|record| record.filed == FiledOutcome::Pending)
    {
        return;
    }

    let project = match store.read(|tx| tx.project_by_uuid(SELF_PROJECT_UUID)) {
        Ok(Some(project)) => project.id,
        Ok(None) => {
            withhold_all_pending(
                &mut ledger,
                &format!(
                    "storyhook's own project (uuid {SELF_PROJECT_UUID}) is not registered in \
                     this store"
                ),
            );
            write_crashes(env, &ledger);
            return;
        }
        // Best-effort: a store error here says nothing about whether the
        // project exists, so nothing is ledgered — every `Pending` record
        // stays `Pending` for the next daemon start to try again.
        Err(_) => return,
    };
    let has_bug_type = store
        .read(|tx| tx.types(project))
        .map(|types| types.iter().any(|t| t.slug == "bug"))
        .unwrap_or(false);

    let mut filed_this_start = 0usize;
    for record in &mut ledger {
        if record.filed != FiledOutcome::Pending {
            continue;
        }
        record.filed = file_one(
            store,
            env,
            project,
            has_bug_type,
            record,
            &mut filed_this_start,
        );
    }
    write_crashes(env, &ledger);
}

/// Sets every still-[`FiledOutcome::Pending`] record's outcome to
/// [`FiledOutcome::Withheld`] with `reason`.
fn withhold_all_pending(ledger: &mut [CrashRecord], reason: &str) {
    for record in ledger.iter_mut() {
        if record.filed == FiledOutcome::Pending {
            record.filed = FiledOutcome::Withheld(reason.to_string());
        }
    }
}

/// Decides one record's fate: withheld, deduped into an existing story, or
/// filed as a new one.
fn file_one<S: Store>(
    store: &S,
    env: &Environment,
    project: ProjectId,
    has_bug_type: bool,
    record: &CrashRecord,
    filed_this_start: &mut usize,
) -> FiledOutcome {
    if !record.classification.is_defect_evidence() {
        return FiledOutcome::Withheld(
            "an unclean exit alone is not evidence of a defect".to_string(),
        );
    }

    let fingerprint_label = format!("{FINGERPRINT_LABEL_PREFIX}{}", fingerprint(record));
    let now = env.now();
    // `store.read`'s closure must return `Result<_, StoreError>`, but
    // `QueryService::search` returns `Result<_, AppError>` — nested rather
    // than converted, so both layers of failure are handled explicitly below
    // instead of losing one to a lossy `From` conversion.
    let existing = match store.read(|tx| {
        let query = QueryService::new(tx, project, &now);
        Ok(query.search(&fingerprint_label))
    }) {
        Ok(Ok(results)) => results
            .into_iter()
            .find(|view| view.story.labels.iter().any(|l| l == &fingerprint_label)),
        Ok(Err(app_error)) => {
            return FiledOutcome::Withheld(format!(
                "could not check for a duplicate story: {app_error}; will retry next start"
            ));
        }
        Err(store_error) => {
            return FiledOutcome::Withheld(format!(
                "could not check for a duplicate story: {store_error}; will retry next start"
            ));
        }
    };

    if let Some(existing) = existing {
        let ctx = build_ctx(store, project, env);
        let comment = format!(
            "This crash was seen again: `{}`, detected {}.",
            record.id, record.detected_at
        );
        return match StoryService::new(&ctx).comment(&existing.story.id, &comment) {
            Ok(_) => FiledOutcome::Deduped(existing.story.id),
            Err(e) => FiledOutcome::Withheld(format!(
                "found the existing story {} but could not comment on it: {e}",
                existing.story.id
            )),
        };
    }

    if *filed_this_start >= MAX_FILED_PER_START {
        return FiledOutcome::Withheld(format!(
            "more than {MAX_FILED_PER_START} new crashes this daemon start; ledgered rather \
             than filed to avoid flooding the tracker"
        ));
    }

    let ctx = build_ctx(store, project, env);
    let input = NewStoryInput {
        title: title_for(record),
        state: None,
        story_type: has_bug_type.then(|| "bug".to_string()),
        description: Some(describe_crash(record)),
        priority: Some("high".to_string()),
        labels: Some(vec![
            CRASH_LABEL.to_string(),
            fingerprint_label,
            AUTO_FILED_LABEL.to_string(),
        ]),
        assignee: None,
        draft: false,
    };
    match StoryService::new(&ctx).create(&input) {
        Ok(snapshot) => {
            *filed_this_start += 1;
            FiledOutcome::Filed(snapshot.id)
        }
        Err(e) => FiledOutcome::Withheld(format!("creating the story failed: {e}")),
    }
}

/// The hook-suppressed, daemon-internal context shape
/// [`crate::daemon::github_poll::poll_github`] and
/// [`crate::service::grouping`]'s own `quiet` already use: no user's hooks
/// fire for a write nobody asked for, and the write carries a provenance a
/// human reading `story log` can attribute to this detector rather than to
/// nothing at all (SH-246).
fn build_ctx<'a, S: Store>(store: &'a S, project: ProjectId, env: &Environment) -> Ctx<'a, S> {
    Ctx::new(store, project, env.home(), env.clone())
        .no_hooks(true)
        .with_provenance(Provenance::command("crash-detector"))
}

/// A crash's fingerprint: 8 hex characters of a SHA-256 digest over the
/// panicking message and source location, or the fatal signal, plus the
/// daemon's version — carried as a label because
/// [`QueryService::search`] does not search descriptions.
///
/// Exact-match rather than normalized: a panic message that embeds a varying
/// value (an index, a byte count) will fingerprint distinctly per value
/// rather than merging with its siblings. That is deliberate — the risk on
/// the other side is two *different* bugs at the same call site collapsing
/// into one dedup, silently hiding one of them from review.
fn fingerprint(record: &CrashRecord) -> String {
    let mut input = String::new();
    match &record.classification {
        CrashClassification::Panicked => {
            if let Some(panic) = record.panics.first() {
                input.push_str(&panic.message);
                if let Some(location) = &panic.location {
                    input.push_str(&location.file);
                    input.push_str(&location.line.to_string());
                }
            }
        }
        CrashClassification::FatalSignal(signal) => {
            input.push_str("fatal-signal-");
            input.push_str(&signal.to_string());
        }
        CrashClassification::UncleanExit => {}
    }
    if let Some(daemon) = &record.daemon {
        input.push_str(&daemon.version);
    }
    let hex = format!("{:x}", Sha256::digest(input.as_bytes()));
    hex[..8].to_string()
}

/// The title a filed story carries.
fn title_for(record: &CrashRecord) -> String {
    match &record.classification {
        CrashClassification::Panicked => {
            let message = record
                .panics
                .first()
                .map(|panic| panic.message.as_str())
                .unwrap_or("unknown panic");
            format!("Daemon panic: {}", truncate_title(message))
        }
        CrashClassification::FatalSignal(signal) => format!("Daemon crash: fatal signal {signal}"),
        CrashClassification::UncleanExit => "Daemon crash: unclean exit".to_string(),
    }
}

/// The first line of `message`, clipped to a length a story list stays
/// scannable at.
fn truncate_title(message: &str) -> String {
    const MAX_CHARS: usize = 100;
    let first_line = message.lines().next().unwrap_or(message);
    if first_line.chars().count() <= MAX_CHARS {
        return first_line.to_string();
    }
    let clipped: String = first_line.chars().take(MAX_CHARS).collect();
    format!("{clipped}…")
}

/// The body a filed story's description carries: what the evidence says,
/// what else was going on, and — bounded and redacted — the crash's own
/// preserved log.
fn describe_crash(record: &CrashRecord) -> String {
    let mut body = String::new();
    match &record.classification {
        CrashClassification::Panicked => {
            body.push_str("The daemon panicked and did not exit cleanly.\n\n");
            if let Some(panic) = record.panics.first() {
                body.push_str(&format!("**Message:** {}\n", panic.message));
                if let Some(location) = &panic.location {
                    body.push_str(&format!(
                        "**Location:** {}:{}:{}\n",
                        location.file, location.line, location.column
                    ));
                }
                body.push_str(&format!("**Thread:** {}\n", panic.thread));
            }
        }
        CrashClassification::FatalSignal(signal) => {
            body.push_str(&format!(
                "The daemon died of signal {signal}, which macOS reported in its own crash \
                 report.\n\n"
            ));
        }
        CrashClassification::UncleanExit => {
            body.push_str("The daemon exited without cleaning up after itself.\n\n");
        }
    }
    if let Some(daemon) = &record.daemon {
        body.push_str(&format!(
            "**Version:** {}\n**Pid:** {}\n**Started:** {}\n",
            daemon.version, daemon.pid, daemon.started_at
        ));
    }
    body.push_str(&format!("**Detected:** {}\n", record.detected_at));
    if !record.inflight.is_empty() {
        body.push_str(&format!(
            "\n**In flight at the time ({}):**\n",
            record.inflight.len()
        ));
        for request in &record.inflight {
            let project = request
                .project
                .as_deref()
                .map(|p| format!(" on `{p}`"))
                .unwrap_or_default();
            body.push_str(&format!("- `{}`{project}\n", request.command));
        }
    }
    if let Some(log_path) = &record.log_path {
        body.push_str(&format!(
            "\n**Preserved log:** `{}`\n\n",
            log_path.display()
        ));
        if let Ok(raw) = std::fs::read_to_string(log_path) {
            body.push_str("```\n");
            body.push_str(&bounded_excerpt(&redact(&raw), MAX_LOG_EXCERPT_BYTES));
            body.push_str("\n```\n");
        }
    }
    body
}

/// The tail of `text`, bounded to `max_bytes` — a panic's message and
/// backtrace land at the *end* of a daemon's stderr, so the tail is the part
/// worth keeping when the whole thing does not fit.
fn bounded_excerpt(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let dropped = text.len() - max_bytes;
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[{dropped} bytes dropped]\n...\n{}", &text[start..])
}

/// GitHub token prefixes SH-153 already had to reckon with, longest first so
/// `github_pat_` is never shadowed by a shorter prefix of it.
const GITHUB_TOKEN_PREFIXES: &[&str] = &["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];

/// Header names whose *value* must never reach a story — this daemon's own
/// bearer token travels in one of these on every `/api/v1/*` request.
const REDACTED_HEADERS: &[&str] = &["Authorization:", "X-Storyhook-Token:"];

/// Strips the shapes this codebase already knows leak into a daemon's stderr
/// — a GitHub token (SH-153) and this daemon's own bearer-token transport
/// headers — before any log text reaches the store.
///
/// Pattern-based rather than value-based, and deliberately so:
/// [`CrashedDaemon`]'s own doc explains why the bearer token this daemon was
/// serving with is never carried past [`harvest`], so there is no specific
/// value here to compare against — only the shapes a leaked one takes.
fn redact(text: &str) -> String {
    let token_free = redact_github_tokens(text);
    token_free
        .lines()
        .map(redact_header_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_github_tokens(text: &str) -> String {
    let mut result = text.to_string();
    for prefix in GITHUB_TOKEN_PREFIXES {
        result = redact_prefixed_run(&result, prefix, "[REDACTED-GITHUB-TOKEN]");
    }
    result
}

/// Replaces every run of `prefix` followed by alphanumerics/underscores with
/// `replacement`, scanning left to right.
fn redact_prefixed_run(text: &str, prefix: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(prefix) {
        out.push_str(&rest[..start]);
        let after_prefix = &rest[start + prefix.len()..];
        let token_end = after_prefix
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(after_prefix.len());
        out.push_str(replacement);
        rest = &after_prefix[token_end..];
    }
    out.push_str(rest);
    out
}

fn redact_header_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    for header in REDACTED_HEADERS {
        let needle = header.to_ascii_lowercase();
        if let Some(pos) = lower.find(&needle) {
            let value_starts = pos + header.len();
            // A trailing space is added unconditionally, not copied from the
            // original: the value after it is always replaced wholesale, so
            // whether the source had zero, one, or several spaces there is
            // irrelevant to what a reader of the redacted line sees.
            return format!("{} [REDACTED]", &line[..value_starts]);
        }
    }
    line.to_string()
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
            filed: FiledOutcome::Pending,
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

    fn panicked_record(message: &str, version: &str) -> CrashRecord {
        CrashRecord {
            id: "id".to_string(),
            detected_at: "2026-01-01T00:00:00Z".to_string(),
            classification: CrashClassification::Panicked,
            daemon: Some(CrashedDaemon {
                pid: 1,
                version: version.to_string(),
                started_at: "2026-01-01T00:00:00Z".to_string(),
            }),
            panics: vec![PanicRecord {
                at: "2026-01-01T00:00:00Z".to_string(),
                pid: 1,
                version: version.to_string(),
                thread: "main".to_string(),
                message: message.to_string(),
                location: Some(PanicLocation {
                    file: "src/daemon/serve.rs".to_string(),
                    line: 42,
                    column: 5,
                }),
            }],
            inflight: Vec::new(),
            log_path: None,
            filed: FiledOutcome::Pending,
        }
    }

    #[test]
    fn is_defect_evidence_is_true_for_panicked_and_fatal_signal_only() {
        assert!(CrashClassification::Panicked.is_defect_evidence());
        assert!(CrashClassification::FatalSignal(libc::SIGSEGV).is_defect_evidence());
        assert!(!CrashClassification::UncleanExit.is_defect_evidence());
    }

    #[test]
    fn fingerprint_is_deterministic_for_the_same_panic() {
        let a = panicked_record("index out of bounds", "2.1.1");
        let b = panicked_record("index out of bounds", "2.1.1");
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn fingerprint_differs_for_a_different_message() {
        let a = panicked_record("index out of bounds", "2.1.1");
        let b = panicked_record("called `unwrap` on a `None` value", "2.1.1");
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn fingerprint_differs_for_a_different_version() {
        let a = panicked_record("index out of bounds", "2.1.1");
        let b = panicked_record("index out of bounds", "2.1.2");
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn fingerprint_is_eight_lowercase_hex_characters() {
        let fp = fingerprint(&panicked_record("oops", "2.1.1"));
        assert_eq!(fp.len(), 8);
        assert!(
            fp.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn title_for_a_panic_names_the_message() {
        let record = panicked_record("index out of bounds: the len is 3", "2.1.1");
        assert_eq!(
            title_for(&record),
            "Daemon panic: index out of bounds: the len is 3"
        );
    }

    #[test]
    fn title_for_a_fatal_signal_names_it() {
        let mut record = panicked_record("unused", "2.1.1");
        record.classification = CrashClassification::FatalSignal(libc::SIGSEGV);
        record.panics.clear();
        assert_eq!(
            title_for(&record),
            format!("Daemon crash: fatal signal {}", libc::SIGSEGV)
        );
    }

    #[test]
    fn truncate_title_leaves_a_short_message_alone() {
        assert_eq!(truncate_title("short"), "short");
    }

    #[test]
    fn truncate_title_clips_a_long_first_line_and_keeps_only_the_first_line() {
        let long = "x".repeat(150);
        let message = format!("{long}\nsecond line never appears");
        let title = truncate_title(&message);
        assert_eq!(title.chars().count(), 101, "100 chars plus the ellipsis");
        assert!(title.ends_with('…'));
        assert!(!title.contains("second line"));
    }

    #[test]
    fn redact_github_tokens_replaces_every_known_prefix() {
        for prefix in GITHUB_TOKEN_PREFIXES {
            let line = format!("using token {prefix}abc123XYZ_789 for the request");
            let redacted = redact(&line);
            assert!(
                !redacted.contains(&format!("{prefix}abc123XYZ_789")),
                "{prefix}: token survived redaction: {redacted:?}"
            );
            assert!(redacted.contains("[REDACTED-GITHUB-TOKEN]"));
        }
    }

    #[test]
    fn redact_stops_a_token_at_the_first_non_token_character() {
        let redacted = redact("token=ghp_abc123, and more text after a comma");
        assert!(redacted.contains("[REDACTED-GITHUB-TOKEN], and more text after a comma"));
    }

    #[test]
    fn redact_strips_authorization_and_storyhook_token_header_values() {
        let redacted = redact("Authorization: Bearer s3cr3t-value-here\nnext line unaffected");
        assert!(!redacted.contains("s3cr3t-value-here"));
        assert!(redacted.contains("Authorization: [REDACTED]"));
        assert!(redacted.contains("next line unaffected"));

        let redacted = redact("X-Storyhook-Token: abcdef0123456789");
        assert!(!redacted.contains("abcdef0123456789"));
        assert!(redacted.contains("X-Storyhook-Token: [REDACTED]"));
    }

    #[test]
    fn redact_is_case_insensitive_on_header_names() {
        let redacted = redact("authorization: Bearer whatever-this-is");
        assert!(!redacted.contains("whatever-this-is"));
    }

    #[test]
    fn redact_leaves_ordinary_text_alone() {
        let text = "the daemon started normally on port 4321";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn bounded_excerpt_leaves_short_text_alone() {
        assert_eq!(bounded_excerpt("short", 100), "short");
    }

    #[test]
    fn bounded_excerpt_keeps_the_tail_and_says_what_it_dropped() {
        let text = "0123456789";
        let excerpt = bounded_excerpt(text, 4);
        assert!(excerpt.contains("6 bytes dropped"));
        assert!(excerpt.ends_with("6789"));
        assert!(!excerpt.contains('0'));
    }

    /// One test, not two: `STORYHOOK_CRASH_FILE` is process-wide state, and
    /// `cargo test` runs test functions concurrently by default — two tests
    /// each mutating and reading it would race each other. Nothing else in
    /// this crate reads the variable, so one sequential test is sufficient
    /// and safe rather than needing a shared lock.
    #[test]
    fn should_file_only_overrides_a_test_build_when_explicitly_told_to() {
        // SAFETY: this is the only test in the crate that touches this
        // variable, and the two mutations below are sequential within it.
        unsafe { std::env::remove_var("STORYHOOK_CRASH_FILE") };
        assert!(
            !should_file(),
            "a test build must never file to a real tracker by default"
        );

        unsafe { std::env::set_var("STORYHOOK_CRASH_FILE", "1") };
        assert!(should_file());

        unsafe { std::env::remove_var("STORYHOOK_CRASH_FILE") };
        assert!(
            !should_file(),
            "removing the override must restore the default"
        );
    }
}
