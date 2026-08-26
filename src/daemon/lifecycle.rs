//! Starting the daemon, finding it, and knowing when it is the wrong one.
//!
//! # Liveness is a held lock, not a pid
//!
//! The daemon takes an exclusive lock on its pidfile and holds it for its whole
//! life, so "is a daemon running" is answered by trying to take that lock — a
//! question with no race in it. The previous answer was `kill -0` against a pid
//! read from a file, which a recycled pid turns into a confident lie, and which
//! reports a wedged process as healthy.
//!
//! # The portfile is authoritative, and it is written after the bind
//!
//! `daemon.json` carries everything a client needs to decide whether to talk to
//! this daemon at all — its port, its version, the executable it was started
//! from and that file's mtime, and the token that authenticates a request to it.
//! It is written **after** the listener is bound, so the port in it is the port
//! the kernel actually gave, and it is written atomically (temp file, `rename`),
//! so a reader never sees half of it.
//!
//! # The spawn lock is held through the child's write
//!
//! A client that decides to spawn takes `daemon.spawn.lock` first, re-checks,
//! and keeps holding it until the child is answering — which means until the
//! child has bound *and* published its portfile. The lock used to be released
//! before the spawn, which left a window in which two clients both decided to
//! start a daemon; auto-spawn would have made that window hot.
//!
//! # Every path here is a path *this store's* daemon owns
//!
//! The portfile, the pidfile, the spawn lock and the log all hang off
//! [`Environment::daemon_state_dir`], which is named after the store's
//! canonical path. Nothing in this module needs to check that a daemon serves
//! the right store, because a client looking for the wrong one would be reading
//! a different directory entirely. That is the fix for SH-123, and it is a
//! construction rather than a check — see `docs/spec/store-isolation.md`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use fs4::FileExt;
use serde::{Deserialize, Serialize};

use crate::env::Environment;
use crate::error::AppError;

/// The RPC protocol version this binary speaks.
///
/// Bumped only when the envelope's *shape* changes. Version skew is enforced on
/// the binary itself rather than on this number — a daemon and a client from
/// different builds never talk — so this exists to make a mismatch describable
/// rather than to make one survivable.
pub const PROTOCOL: u32 = 1;

/// How long a client waits for a daemon it just spawned to answer.
pub const SPAWN_DEADLINE: Duration = Duration::from_secs(5);

/// How often it asks, while waiting.
const SPAWN_POLL: Duration = Duration::from_millis(25);

/// One request the daemon is serving right now.
///
/// **The record whose motion is the client's clock.** Published when a
/// request's envelope has parsed and removed when its answer is ready, so the
/// set it belongs to changes exactly when the daemon finishes something — and
/// a client waiting on a command it cannot see any bytes from can therefore
/// tell "the daemon is working through a queue" from "the daemon has stopped
/// finishing things" without asking it anything (SH-144).
///
/// Every field answers one question the failure message has to answer.
/// `request_id` says *is this mine*; `command` names the work;
/// `served_deadline_secs` says how long this daemon commits to spending on
/// it; `project` says whose; `pid` is the way out, because `story daemon
/// stop` has no signal fallback; `started_at` says how long.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CurrentRequest {
    /// The envelope's own id, which is how a client recognises its own request.
    pub request_id: String,
    /// The invocation's verb, as [`crate::invoke::invocation_name`] spells it.
    pub command: String,
    /// The project the command resolved to, if it named one.
    pub project: Option<String>,
    /// The daemon's process id — the remedy a wedged daemon leaves.
    pub pid: u32,
    /// When the daemon picked this request up, RFC 3339.
    pub started_at: String,
    /// How long this daemon commits to spending on this command before a
    /// client gives up on it — its own, if it is theirs, or the most patient
    /// deadline in the set, if they are only queued behind it. Computed once,
    /// by [`served_deadline`], at the moment this record is named: only the
    /// daemon knows the command's own `cwd`, and therefore its project's hook
    /// configuration, so only the daemon can derive this honestly (SH-173). A
    /// client reads it rather than recomputing a guess, which is what makes
    /// the number the same whether the record is theirs or somebody else's.
    pub served_deadline_secs: u64,
    /// The client's own working directory, as it named it in the envelope.
    /// Empty for a REST/dashboard job, which has no directory of its own to
    /// name. The one field `migrate`'s own concurrency check
    /// (`crate::api::rpc::concurrency_conflict`) can key on: `migrate` mints
    /// the project it would otherwise be scoped to, so `cwd` — the directory
    /// actually being migrated — is the only thing two concurrent instances
    /// can be compared by.
    pub cwd: PathBuf,
}

/// Writes the file a daemon's clients read, from `entries` in the order
/// given — a JSON array, removed when empty and never written as `[]`, so
/// "absent" keeps meaning "nothing in flight" exactly as when this file held
/// at most one bare object. Public so a test fixture can plant a record
/// directly, the way a direct call to the single-object writer this replaced
/// used to.
///
/// Temp-plus-`rename` and mode 0600, the same shape as [`write_info`] and for
/// the same reason: a client that read a half-written file would see a
/// `request_id` that matches nothing and conclude it was queued when it is
/// being served.
///
/// **Best-effort, and never fatal.** A daemon whose state directory is full or
/// read-only must still run the command it was asked to run — failing here
/// would replace a good answer with a bad diagnostic about a diagnostic, which
/// is the rule [`record_startup_failure`] already states for the same reason.
pub fn publish_inflight(env: &Environment, entries: &[CurrentRequest]) {
    let path = env.daemon_current();
    if entries.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let Ok(document) = serde_json::to_string(entries) else {
        return;
    };
    let temp = path.with_extension("json.tmp");

    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let Ok(mut file) = options.open(&temp) else {
        return;
    };
    if file.write_all(document.as_bytes()).is_err() {
        return;
    }
    drop(file);
    let _ = std::fs::rename(&temp, &path);
}

/// Reads everything a daemon says it is serving, in arrival order.
///
/// Empty covers three states a caller must not conflate, exactly as an
/// absent record used to (SH-144): the daemon is between requests, it never
/// published at all (see SH-144's "the record never appears" branch), or the
/// file is being replaced right now. The last is why a parse failure reads
/// as empty rather than as an error: the write is atomic, so a torn read is
/// not possible, but a *stale* document from an older binary's format is —
/// and `usable` has already refused that daemon before this is ever called.
#[must_use]
pub fn read_inflight(env: &Environment) -> Vec<CurrentRequest> {
    let Ok(raw) = std::fs::read_to_string(env.daemon_current()) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Everything this daemon is serving right now, reached through [`Self::enter`]
/// rather than through [`publish_inflight`] directly.
///
/// **One writer of the set**, which is what [`Environment::daemon_current`]
/// asks for. A raw `publish_inflight` call, made by two callers at once, lets
/// the second's write clobber the first's — the exact defect concurrent
/// dispatch (SH-173) would otherwise reintroduce. Routing every writer
/// through one `InFlight` closes that: [`Self::enter`] hands out a slot, and
/// only the guard that owns a slot can name or clear it.
pub struct InFlight {
    entries: std::sync::Mutex<std::collections::BTreeMap<u64, CurrentRequest>>,
    next: std::sync::atomic::AtomicU64,
    env: Environment,
}

impl InFlight {
    /// A registry with nothing in flight, for the daemon rooted at `env`.
    #[must_use]
    pub fn new(env: Environment) -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            next: std::sync::atomic::AtomicU64::new(0),
            env,
        }
    }

    /// Clears whatever this record said before this daemon existed.
    ///
    /// Call once, before any [`Self::enter`] — [`serve`](crate::daemon::serve::serve)
    /// does this immediately after building its `InFlight`, ahead of `ready()`
    /// and therefore ahead of any listener accepting a request a client could
    /// poll this record from. At that moment nothing can legitimately be in
    /// flight yet, so a non-empty record can only be one a *previous* daemon
    /// left behind: an orderly shutdown always empties it (the last open
    /// [`Entry`]'s drop calls [`Self::leave`], which publishes an empty set),
    /// so surviving to here means that daemon did not exit in an orderly way
    /// — `kill -9`, a crash.
    ///
    /// Left alone, this is a live defect: the next client to wait on this new
    /// daemon reads a frozen record naming a command that finished long ago,
    /// and gives up after its deadline having learned nothing true. Best
    /// effort, like every other write this type performs — a state directory
    /// this daemon cannot write to is a daemon that should still start.
    ///
    /// Whatever is found is also the ledger's problem now: it did not finish
    /// and nobody confirmed whether its write landed, which is exactly what
    /// [`ledger_abandoned`] exists to surface to `story doctor`.
    pub fn harvest_stale(&self) {
        let stale = read_inflight(&self.env);
        ledger_abandoned(
            &self.env,
            &stale,
            "found still in flight when this daemon started; the daemon that \
             named it did not exit normally",
        );
        publish_inflight(&self.env, &[]);
    }

    /// How many named entries are open right now — what a shutdown drains
    /// to zero before this daemon exits.
    ///
    /// An unnamed slot (`hello`, `shutdown`) never counts: it was never
    /// inserted, so there is nothing here for a drain to wait on.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing is named right now.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Opens a slot for one request. The slot is unnamed — and therefore
    /// invisible to a reader of the file — until [`Entry::name`] is called;
    /// closing the returned guard (on drop, including on a panic) retracts it.
    ///
    /// Reserving a slot is one atomic increment, no I/O: a caller that never
    /// names its entry (`hello`, `shutdown` — see [`crate::api::rpc::route`])
    /// costs nothing beyond the counter.
    #[must_use]
    pub fn enter(&self) -> Entry<'_> {
        let slot = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Entry {
            inflight: self,
            slot,
        }
    }

    fn amend(&self, slot: u64, what: CurrentRequest) {
        let mut entries = self.lock();
        entries.insert(slot, what);
        self.publish(&entries);
    }

    /// Removes a slot. A no-op removal (the slot was never named) republishes
    /// nothing — the file a reader sees is unaffected by a request that never
    /// named itself.
    fn leave(&self, slot: u64) {
        let mut entries = self.lock();
        if entries.remove(&slot).is_some() {
            self.publish(&entries);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<u64, CurrentRequest>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Writes the file this daemon's clients read, from the current set of
    /// named entries, in arrival order (a `BTreeMap` keyed by monotonically
    /// increasing slot).
    fn publish(&self, entries: &std::collections::BTreeMap<u64, CurrentRequest>) {
        let ordered: Vec<CurrentRequest> = entries.values().cloned().collect();
        publish_inflight(&self.env, &ordered);
    }
}

/// A guard over one open slot in [`InFlight`]. Closes the slot on drop —
/// including on an unwind — so a panicked handler cannot leave a stale record
/// behind the way a bare `clear_current` call, skipped by an early return,
/// could.
pub struct Entry<'a> {
    inflight: &'a InFlight,
    slot: u64,
}

impl Entry<'_> {
    /// Names the work this slot is doing. Called once an envelope has parsed
    /// — the moment a record becomes worth reading at all (`crate::api::rpc`'s
    /// own reasoning for publishing where it does, unchanged by this type).
    pub fn name(&self, what: CurrentRequest) {
        self.inflight.amend(self.slot, what);
    }
}

impl Drop for Entry<'_> {
    fn drop(&mut self) {
        self.inflight.leave(self.slot);
    }
}

/// A request abandoned mid-flight — forced out by `story daemon stop
/// --force`, or found already abandoned when the next daemon started
/// ([`InFlight::harvest_stale`]) because the one that named it did not exit
/// normally.
///
/// Kept as its own file rather than folded into the store: the daemon may be
/// dying while it holds `write_lock`, so anything written on this path has
/// to work without the store's cooperation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AbandonedRequest {
    /// What was abandoned. The whole record, not a summary — `story doctor
    /// abandoned` is what a human reads to decide whether to redo it.
    #[serde(flatten)]
    pub request: CurrentRequest,
    /// RFC 3339, when this daemon concluded the request was abandoned — not
    /// when the work itself actually stopped, which is unknowable from here.
    pub abandoned_at: String,
    /// Human-readable: which path recorded this, and why.
    pub reason: String,
}

/// Reads the abandoned-work ledger, oldest first.
#[must_use]
pub fn read_abandoned(env: &Environment) -> Vec<AbandonedRequest> {
    let Ok(raw) = std::fs::read_to_string(env.daemon_abandoned()) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Appends `entries` to the abandoned-work ledger, each stamped with `reason`
/// and the current time. A no-op on an empty slice, so a healthy stop or
/// start never touches this file at all.
///
/// Best-effort, like every other write in this module: a ledger this daemon
/// cannot write to must not stop it from stopping, or from starting — losing
/// the *diagnostic* is a strictly smaller failure than losing the *work*.
pub fn ledger_abandoned(env: &Environment, entries: &[CurrentRequest], reason: &str) {
    if entries.is_empty() {
        return;
    }
    let abandoned_at = env.now();
    let mut ledger = read_abandoned(env);
    ledger.extend(entries.iter().cloned().map(|request| AbandonedRequest {
        request,
        abandoned_at: abandoned_at.clone(),
        reason: reason.to_string(),
    }));
    write_abandoned(env, &ledger);
}

/// Forgets one abandoned entry by request id, or every entry when `request_id`
/// is `None` — `story doctor abandoned clear` and `--all` respectively.
/// Returns whether anything was actually removed, so the caller can say so.
pub fn clear_abandoned(env: &Environment, request_id: Option<&str>) -> bool {
    let ledger = read_abandoned(env);
    let kept: Vec<AbandonedRequest> = match request_id {
        Some(id) => ledger
            .iter()
            .filter(|entry| entry.request.request_id != id)
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    let changed = kept.len() != ledger.len();
    if changed {
        write_abandoned(env, &kept);
    }
    changed
}

fn write_abandoned(env: &Environment, ledger: &[AbandonedRequest]) {
    let path = env.daemon_abandoned();
    if ledger.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let Ok(document) = serde_json::to_string(ledger) else {
        return;
    };
    let temp = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let Ok(mut file) = options.open(&temp) else {
        return;
    };
    if file.write_all(document.as_bytes()).is_err() {
        return;
    }
    drop(file);
    let _ = std::fs::rename(&temp, &path);
}

/// What a running daemon publishes about itself.
///
/// Every field is here to answer one question a client has before it sends
/// anything: *where* (`port`), *is it mine* (`version`, `exe`, `exe_mtime`),
/// *may I* (`token`), and *what is it* (`pid`, `started_at`, `protocol`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    /// The daemon's process id. Diagnostic only — liveness comes from the lock.
    pub pid: u32,
    /// The loopback port it is answering on.
    pub port: u16,
    /// The `storyhook` version it was built from.
    pub version: String,
    /// The RPC protocol it speaks.
    pub protocol: u32,
    /// The executable it is running.
    pub exe: PathBuf,
    /// That executable's modification time, in seconds since the epoch.
    ///
    /// Version alone is not enough: a developer rebuilding the same version is
    /// the common case, and a daemon still serving the previous build of it is
    /// the 42-hour-stale-dashboard bug that started this.
    pub exe_mtime: i64,
    /// When it started, RFC3339.
    pub started_at: String,
    /// The bearer token every RPC request must carry.
    pub token: String,
    /// The store this daemon holds open, canonicalized.
    ///
    /// **Not the enforcement mechanism** — that is the keyed directory this
    /// file lives in, which a client serving another store never opens. It is
    /// here so a portfile is self-describing when a human reads it, and so a
    /// collision on the key digest would be detectable rather than silent.
    ///
    /// Defaulted rather than required, because a portfile written before this
    /// field existed has to *parse* in order to be stood down; it reads as an
    /// empty path, which [`Self::serves`] answers `false` for.
    #[serde(default)]
    pub store_path: PathBuf,
    /// The tailnet interface this daemon bound, if it bound one.
    ///
    /// `None` means loopback only — the honest answer both for a machine with
    /// no tailnet and for a probe that missed its deadline. In either case
    /// there is no tailnet listener, so there is nothing to advertise.
    ///
    /// Written from the same [`crate::daemon::serve::BoundAddress`] that
    /// produced this daemon's trusted-host allowlist, so what it advertises and
    /// what it trusts cannot disagree. They used to: every client re-derived
    /// the host by probing the machine, which answers a different question
    /// (SH-110).
    ///
    /// Defaulted for the same reason `store_path` is: an older portfile has to
    /// *parse* in order to be stood down, and [`read_info_at`] treats one it
    /// cannot parse as absent — which is the one state `stop` cannot resolve.
    /// It reads as `None`, i.e. loopback, which is always true.
    #[serde(default)]
    pub tailnet: Option<crate::daemon::tailnet::TailnetBind>,
    /// The `Set-Cookie` name a browser holds a named token in for this store
    /// (SH-255) — `crate::api::tokens::cookie_name`, published so the CLI and
    /// e2e suite read it here rather than recompute the digest themselves.
    ///
    /// Defaulted for the reason `store_path` and `tailnet` are: an older
    /// portfile has to parse in order to be stood down. An empty string reads
    /// as "unknown," which is the honest answer for a daemon that predates
    /// this field.
    #[serde(default)]
    pub cookie_name: String,
}

impl DaemonInfo {
    /// Whether this daemon holds `store`.
    ///
    /// The last line of defence rather than the first: a client only ever
    /// reads a portfile from its own store's directory, so a `false` here means
    /// two canonical paths produced one key, and the caller should treat the
    /// daemon as somebody else's.
    pub fn serves(&self, store: &Path) -> bool {
        self.store_path == store
    }

    /// Whether this daemon is running the same build as the current process.
    ///
    /// Three-part: the version, the path of the executable, and that file's
    /// modification time. A mismatch on any of them means the daemon is serving
    /// code the caller is not running, and the caller must not use it.
    pub fn is_this_binary(&self) -> bool {
        let Ok((exe, mtime)) = current_binary() else {
            return false;
        };
        self.version == env!("CARGO_PKG_VERSION") && self.exe == exe && self.exe_mtime == mtime
    }

    /// The loopback address this daemon answers on.
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// Every address this daemon reported binding.
    pub fn bound(&self) -> crate::daemon::serve::BoundAddress {
        crate::daemon::serve::BoundAddress {
            loopback: self.addr(),
            tailnet: self.tailnet.clone(),
        }
    }

    /// The host to show or copy for reaching this daemon: its tailnet's
    /// advertised name when it bound one, else loopback.
    ///
    /// Always a host this daemon is actually listening on. That is the whole
    /// point — it is read from what the daemon published, never derived by
    /// probing the machine the reader happens to be running on.
    pub fn advertised_host(&self) -> String {
        self.bound().advertise_host()
    }

    /// The dashboard URL to show or copy.
    pub fn dashboard_url(&self) -> String {
        self.bound().dashboard_url()
    }
}

/// This executable's path and modification time.
fn current_binary() -> Result<(PathBuf, i64), AppError> {
    let exe = std::env::current_exe()
        .map_err(|e| AppError::Storage(format!("failed to find the running executable: {e}")))?;
    let mtime = std::fs::metadata(&exe)
        .and_then(|meta| meta.modified())
        .map_err(|e| AppError::Storage(format!("failed to stat {}: {e}", exe.display())))?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok((exe, mtime))
}

/// Reads the portfile, or `None` if there is not a readable one.
///
/// A portfile this build cannot parse is treated as absent rather than as an
/// error: it was written by some other version, which is precisely the case
/// where the right move is to start a daemon of our own.
pub fn read_info(env: &Environment) -> Option<DaemonInfo> {
    read_info_at(&env.daemon_file())
}

/// Reads a portfile from a named path, or `None` if there is not a readable one.
pub fn read_info_at(path: &Path) -> Option<DaemonInfo> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Writes the portfile atomically, readable only by its owner.
///
/// Temp file plus `rename`, because a client reading a half-written portfile
/// would either fail to parse it (harmless) or read a stale port beside a fresh
/// token (not). Mode 0600 because the file carries a bearer token for a
/// full-privilege API.
///
/// `pub(crate)` rather than private: [`crate::daemon::crash`]'s own tests use
/// it to fabricate a residue portfile, the same fixture `stopping_clears_a_
/// portfile_left_by_a_crashed_daemon` below builds for itself.
pub(crate) fn write_info(env: &Environment, info: &DaemonInfo) -> Result<(), AppError> {
    std::fs::create_dir_all(env.daemon_state_dir())?;
    let final_path = env.daemon_file();
    let temp_path = final_path.with_extension("json.tmp");

    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(&temp_path)
        .map_err(|e| AppError::Storage(format!("failed to write the daemon portfile: {e}")))?;
    file.write_all(serde_json::to_string_pretty(info)?.as_bytes())?;
    file.sync_all()?;
    drop(file);

    std::fs::rename(&temp_path, &final_path)
        .map_err(|e| AppError::Storage(format!("failed to publish the daemon portfile: {e}")))?;
    Ok(())
}

/// Opens the pidfile for locking, creating it and its directory if needed.
fn open_pidfile(env: &Environment) -> Result<File, AppError> {
    std::fs::create_dir_all(env.daemon_state_dir())?;
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(env.daemon_pidfile())
        .map_err(|e| AppError::Storage(format!("failed to open the daemon pidfile: {e}")))
}

/// Whether a daemon currently holds the pidfile lock.
///
/// The replacement for `is_process_alive`, and strictly better than it: a lock
/// is held by a *process*, so a recycled pid cannot impersonate one, and a
/// daemon that has exited releases it whether or not it got to clean up.
pub fn is_live(env: &Environment) -> bool {
    let Ok(file) = open_pidfile(env) else {
        return false;
    };
    match file.try_lock_exclusive() {
        // Nobody held it, so nobody is running. Release what we just took.
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

/// The lock a daemon holds for its whole life.
///
/// Returned rather than dropped: the caller keeps it alive, and the lock is
/// released when the process ends, however it ends.
pub fn claim_pidfile(env: &Environment) -> Result<File, AppError> {
    let file = open_pidfile(env)?;
    file.try_lock_exclusive().map_err(|_| {
        AppError::Usage(
            "a storyhook daemon is already running. Run `story daemon stop` first.".to_string(),
        )
    })?;
    Ok(file)
}

/// A fresh bearer token for one daemon's lifetime.
///
/// A v4 UUID's 122 random bits come from the operating system's CSPRNG, which is
/// what a bearer token needs and the only property that matters here. Written in
/// simple form so it can go in a header without escaping.
///
/// `pub(crate)`: [`super::serve::bind_and_serve`] (the `test-seam`-gated
/// harness entry point) needs one too, now that SH-187 makes every `/api/**`
/// route require a real token — an empty one, which that seam used to pass,
/// can no longer authenticate anything ([`crate::api::rpc::token_ok`] fails
/// closed on it).
pub(crate) fn mint_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Builds the portfile contents for a daemon that has just bound `bound` over
/// `store`.
pub fn info_for(
    bound: &super::serve::BoundAddress,
    token: String,
    now: &str,
    store: &Path,
) -> Result<DaemonInfo, AppError> {
    let (exe, exe_mtime) = current_binary()?;
    Ok(DaemonInfo {
        pid: std::process::id(),
        port: bound.port(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: PROTOCOL,
        exe,
        exe_mtime,
        started_at: now.to_string(),
        token,
        store_path: store.to_path_buf(),
        tailnet: bound.tailnet.clone(),
        cookie_name: crate::api::tokens::cookie_name_for_store(store),
    })
}

/// Binds a listener for the daemon: the preferred port, else one the kernel
/// picks.
///
/// Falling back rather than failing is what makes "port already in use" stop
/// being a startup failure at all. The portfile is authoritative about where the
/// daemon actually is, so nothing downstream needs the preferred port to have
/// been available.
///
/// SH-147 was filed speculatively — worried this could call the tailnet
/// probe inside [`super::serve::bind_listeners`] twice, spending `2 *
/// TAILNET_PROBE_TIMEOUT` inside [`SPAWN_DEADLINE`] on a fallback — and was
/// never reproduced, because `bind_listeners` binds loopback before it
/// probes, so a port-taken failure returns before the probe is ever reached.
/// SH-186 removed the concern at its root rather than merely confirming that
/// ordering: `bind_listeners` no longer probes the tailnet at all, on either
/// call here, so there is no probe on this path to double.
/// `tests/tailnet_probe_budget.rs` now pins the stronger claim.
pub fn bind_preferred(
    env: &Environment,
) -> Result<(Vec<super::serve::Listener>, super::serve::BoundAddress), AppError> {
    let preferred = env.preferred_port();
    match super::serve::bind_listeners(preferred) {
        Ok(bound) => Ok(bound),
        Err(_) if preferred != 0 => super::serve::bind_listeners(0),
        Err(e) => Err(e),
    }
}

/// Runs the daemon in this process, until it is asked to stop.
///
/// The order is the contract: install the panic hook (so nothing this process
/// does from here on can panic unrecorded), take the lifetime lock (so a
/// second daemon cannot start), harvest whatever residue that lock's previous
/// holder left (SH-287 — the portfile still names *them*, and `write_info`
/// below is about to overwrite it), bind loopback (so the port is real),
/// publish the portfile (so a client can find it), then serve — and only
/// from a background thread inside `serve`, after all of that, does anything
/// ever probe the tailnet (SH-186). No step before `serve` waits on
/// `tailscale` in any way.
pub fn run<S: crate::store::Store>(store: &S, env: &Environment) -> Result<(), AppError> {
    crate::daemon::crash::install_panic_hook(env);
    let _pidfile = claim_pidfile(env)?;
    crate::daemon::crash::harvest(env);
    let (listeners, bound) = bind_preferred(env)?;
    let info = info_for(&bound, mint_token(), &env.now(), env.store_path())?;
    write_info(env, &info)?;
    eprintln!(
        "storyhook daemon {} on http://127.0.0.1:{} (pid {}) holding {}",
        info.version,
        info.port,
        info.pid,
        info.store_path.display()
    );

    // Before serving anything: one global database is one global blast radius,
    // and a daemon start is the only moment on this machine that reliably
    // happens both after a reboot and after every upgrade. A backup that cannot
    // be taken is reported and not fatal — a machine with a read-only backup
    // directory should still get a tracker.
    match crate::daemon::backup::run_if_due(store, env) {
        Ok(Some(path)) => eprintln!("storyhook daemon: wrote a backup to {}", path.display()),
        Ok(None) => {}
        Err(e) => eprintln!("warning: storyhook could not write a backup: {e}"),
    }

    let bus = crate::daemon::bus::ChangeBus::new();
    let (info_for_late_bind, env_for_late_bind) = (info.clone(), env.clone());
    super::serve::serve(
        store,
        env,
        listeners,
        bound.trusted_hosts(),
        bus,
        info.token.clone(),
        || {},
        // Fires at most once, from the background thread that binds the
        // tailnet interface (SH-146's self-heal path, and since SH-186 the
        // *only* path — every tailnet bind is "late" relative to `serve`
        // starting, including the first). Without this the portfile would
        // keep reading `tailnet: None` forever after a bind that happened
        // off this thread, so `story daemon status`/`address` — reading the
        // same file — would still report the daemon as loopback-only.
        move |bound: &super::serve::BoundAddress| {
            let mut info = info_for_late_bind.clone();
            info.tailnet = bound.tailnet.clone();
            if let Err(e) = write_info(&env_for_late_bind, &info) {
                eprintln!(
                    "warning: bound the tailnet interface but could not update the daemon \
                     portfile: {e}"
                );
            }
        },
    )
}

/// Finds a daemon this binary can talk to, starting one if there is not one.
///
/// The fast path costs no round trip: a portfile whose version, executable and
/// mtime match this build describes a daemon we may use, and the lock says
/// whether it is still there. Everything else goes through the slow path, which
/// takes the spawn lock before deciding anything, so two clients racing to start
/// a daemon produce one daemon.
pub fn ensure(env: &Environment) -> Result<DaemonInfo, AppError> {
    if let Some(info) = usable(env) {
        return Ok(info);
    }
    spawn_locked(env)
}

/// The daemon in this store's directory, if it is one this client may use.
///
/// Three questions, and all three must answer yes: is a portfile there, was it
/// written by this build, and is a process still holding the lifetime lock.
///
/// The store check is the fourth, and it is redundant *by design*: this portfile
/// was read out of a directory named after the store's own digest, so a daemon
/// naming a different store means two canonical paths produced one key. That
/// cannot be allowed to resolve as "close enough" — it is the exact shape of
/// SH-123 — so the answer is to treat the daemon as a stranger's and start our
/// own, which is what every other mismatch here does.
fn usable(env: &Environment) -> Option<DaemonInfo> {
    let info = read_info(env)?;
    (info.is_this_binary() && info.serves(env.store_path()) && is_live(env)).then_some(info)
}

/// How long a client waits to take the daemon spawn lock.
///
/// **Derived, never chosen**, and `the_spawn_lock_bound_is_the_sum_of_what_it_waits_on`
/// fails if the arithmetic below stops matching the code. The holder of this
/// lock can legitimately spend three [`CONTROL_DEADLINE`]s inside it —
/// [`request_shutdown`] against a stale daemon, then [`hello`] and another
/// `request_shutdown` inside [`stand_down_legacy_daemon`] — and three
/// [`SPAWN_DEADLINE`]s — two [`wait_until`] calls and [`await_healthy`]. A
/// waiter that gave up before that sum could abort a spawn that was going to
/// succeed, which is a worse defect than the one this bound exists to fix.
///
/// The two branches look mutually exclusive and are not: a daemon that answers
/// its shutdown late clears its own portfile on the way out, which re-opens
/// `stand_down_legacy_daemon`'s `daemon_file().exists()` guard behind it.
///
/// Public because `tests/concurrency_soak.rs` derives its own per-command
/// deadline from this one rather than restating a number. The two have to stay
/// ordered — a client that exhausts this bound and correctly reports so must not
/// race the harness meant to outlive it — and an import is the only form of that
/// promise which cannot drift.
pub const SPAWN_LOCK_DEADLINE: Duration =
    Duration::from_secs(3 * CONTROL_DEADLINE.as_secs() + 3 * SPAWN_DEADLINE.as_secs());

/// Takes the spawn lock within [`SPAWN_LOCK_DEADLINE`], or gives up loudly.
///
/// **A poll rather than a block, and that is forced rather than chosen.** `fs4`
/// offers exactly six locking primitives — `lock_shared`, `lock_exclusive`,
/// `try_lock_shared`, `try_lock_exclusive`, `unlock` and `allocate` — and none
/// of them is a timed acquire, so a bounded wait can only be built out of the
/// non-blocking one. `flock(2)` would not have helped either: it reports no
/// waiter count, no queue position and no ordering guarantee, which is why the
/// "queue-position report" SH-143 asked for cannot be built over this lock at
/// all (SH-143).
///
/// The cost is that acquisition becomes barging rather than whatever ordering
/// the kernel was giving blocked waiters. That is affordable only because
/// [`spawn_locked`] publishes its verdict: holds collapse to microseconds once
/// a waiter can adopt an answer instead of recomputing it.
fn acquire_spawn_lock(path: &Path, deadline: Duration) -> Result<File, AppError> {
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| AppError::Storage(format!("failed to open the daemon spawn lock: {e}")))?;

    let started = Instant::now();
    let mut announced = false;
    loop {
        if lock.try_lock_exclusive().is_ok() {
            return Ok(lock);
        }
        let waited = started.elapsed();
        if waited >= deadline {
            return Err(AppError::Storage(format!(
                "gave up after {}s waiting for the daemon spawn lock. Another storyhook \
                 client holds {} and has not finished starting a daemon in the longest \
                 time that can legitimately take, so it is stuck rather than slow.",
                deadline.as_secs(),
                path.display(),
            )));
        }
        // Once, and only to a human. A wait this long is worth explaining, but
        // `tests/cli_error_streams.rs` pins an empty stderr both for a
        // successful command and for any command under `--json`, so anything
        // unconditional here would break a contract the suite already holds.
        // A pipe gets silence and the same exit code it always got.
        if !announced && waited >= SPAWN_DEADLINE {
            announced = true;
            announce_waiting(path);
        }
        std::thread::sleep(SPAWN_POLL);
    }
}

/// Tells a human at a terminal that this wait is a queue rather than a hang.
/// Names a login agent that is broken or points somewhere else — on the one
/// path where that fact has just cost the operator something.
///
/// This client is about to *spawn* a daemon, which on a machine whose login
/// agent works should almost never happen: launchd started one at login and
/// every client since has found it usable. Reaching here means either this is
/// the first daemon since boot on a machine with no agent (nothing to say), or
/// the agent that was supposed to have started one did not. So this fires at
/// most once per daemon lifetime and essentially never on a healthy machine,
/// which is what keeps it clear of SH-396's self-noise shape — a gate that
/// repeats one unchanged fact until nobody reads it.
///
/// Two conditions, each load-bearing:
///
/// * **It warns and never refuses.** `ensure` is availability plumbing;
///   refusing to start a daemon over a fact about a *launchd agent* would take
///   the tracker down for something with no bearing on the daemon this call is
///   about. That trade — a silent non-start in exchange for a rare wrong
///   migration — is the one SH-411's council eliminated a whole candidate for.
/// * **Only a terminal.** [`crate::invoke::HttpInvoker`] re-enters `ensure`
///   from inside a live TUI session, and `tests/cli_error_streams.rs` pins
///   stderr silent on success — the same guard [`announce_waiting`] takes, for
///   both of the same reasons.
///
/// No longer gated on the default store (SH-414): `health(env)` now reads
/// *that store's own* login agent, so a `--store-path` client at a terminal
/// has exactly the stake a store-scoped guard would have denied it. Before
/// SH-414 every non-default store's agent shared the default store's label,
/// so this guard reporting on someone else's agent would have been reporting
/// on the very agent it was told to ignore.
fn note_stale_login_agent(env: &Environment) {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return;
    }
    if let Some(said) = crate::daemon::agent::warning(&crate::daemon::agent::health(env)) {
        eprintln!("warning: {said}");
    }
}

fn announce_waiting(path: &Path) {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprintln!(
            "storyhook: waiting for another client to finish starting the daemon ({})",
            path.display()
        );
    }
}

/// The verdict of an attempt that concluded *while this client was waiting*.
///
/// The freshness rule is the whole safety of adopting somebody else's answer,
/// and it is one comparison: the record has to have been written **after this
/// client arrived**. A record older than the arrival describes a world this
/// client has not seen and must not be believed — the daemon it failed against
/// may since have been stopped by hand, or the binary rebuilt.
///
/// Modification time rather than a timestamp inside the document, because the
/// two would be two answers to one question and only one of them is written by
/// something other than the process being trusted.
fn attempt_verdict(env: &Environment, arrived: std::time::SystemTime) -> Option<AppError> {
    let path = env.daemon_attempt();
    let finished = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()?;
    if finished < arrived {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<crate::error::WireError>(&raw)
        .ok()
        .map(|wire| {
            AppError::from(wire).with_context(
                "another storyhook client was already starting the daemon while this \
                 command waited for it, and this is what happened:",
            )
        })
}

/// Publishes what an attempt concluded, for the clients queued behind it.
///
/// Written on failure and **removed on success**, because success needs no
/// record: a waiter's own `usable` re-check finds the daemon that now exists.
/// Leaving a spent failure behind would be a claim about a world that has since
/// been fixed.
///
/// Temp-plus-rename like the portfile, so a waiter never reads half a document —
/// and to a *sibling* path, never over `daemon.spawn.lock` itself, whose `flock`
/// lives on the inode a rename would replace.
///
/// Best-effort throughout: a client that cannot write this file has still done
/// its real job, and failing here would replace a good diagnosis with a bad one
/// about a diagnostic.
fn publish_attempt(env: &Environment, outcome: &Result<DaemonInfo, AppError>) {
    let path = env.daemon_attempt();
    let Err(failure) = outcome else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    let Ok(document) = serde_json::to_string(&crate::error::WireError::from(failure)) else {
        return;
    };
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, document).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// The slow path: hold the spawn lock, re-check, replace what is there, start a
/// daemon, and keep holding until it answers.
///
/// # A waiter adopts rather than repeats
///
/// The queue behind this lock used to be one *attempt* deep per client: four
/// clients arriving at once against a wedged daemon took 10.16s, 20.25s, 30.33s
/// and 40.42s, each one paying in full to recompute a verdict the first had
/// already reached. It is one attempt deep in total now. A client records when
/// it arrived, and if the holder ahead of it published a verdict *after* that
/// moment, it takes that answer instead of spending another ten seconds
/// discovering it (SH-143).
fn spawn_locked(env: &Environment) -> Result<DaemonInfo, AppError> {
    std::fs::create_dir_all(env.daemon_state_dir())?;
    // Taken before the wait, so "did this verdict arrive after I got here" is a
    // question about the whole wait rather than about the instant it ended.
    let arrived = std::time::SystemTime::now();
    let lock = acquire_spawn_lock(&env.daemon_spawn_lock(), SPAWN_LOCK_DEADLINE)?;

    // Re-check under the lock: another client may have started one while this
    // one was waiting for the lock, and starting a second would be the whole
    // point of the lock being missed.
    if let Some(info) = usable(env) {
        let _ = FileExt::unlock(&lock);
        return Ok(info);
    }
    // Nothing usable, and somebody just failed to make one while we waited.
    // Returned *without* republishing: the record describes a real attempt, so
    // an adoption must not refresh its timestamp, or a steady arrival of clients
    // would keep handing the same stale verdict forward and nobody would ever
    // try again.
    if let Some(adopted) = attempt_verdict(env, arrived) {
        let _ = FileExt::unlock(&lock);
        return Err(adopted);
    }

    note_stale_login_agent(env);

    let outcome = (|| -> Result<DaemonInfo, AppError> {
        // Something is there that is not ours. Ask it to stand down before
        // taking its place: it holds the pidfile lock, and a new daemon cannot
        // start while it does.
        //
        // **A refusal here is the most actionable fact this function learns,
        // and it used to be discarded.** A daemon answers a shutdown request
        // *before* it exits (`serve.rs`), so a request that did not come back
        // is one the incumbent never accepted — it is wedged, not merely slow.
        // What arrived instead was the consequence: the child fails to claim
        // the pidfile and reports "a storyhook daemon is already running. Run
        // `story daemon stop` first", which is a remedy that will itself hang
        // against a daemon in this state. Kept so the report names the cause
        // above the consequence.
        //
        // The wait after it stays, deliberately, and `wait_until` short-circuits
        // the moment the incumbent lets go — so the full five seconds are only
        // ever paid on a path that is going to fail anyway, and paying them buys
        // the case where a daemon busy rather than wedged finishes and exits.
        let refusal = if let Some(stale) = read_info(env)
            && is_live(env)
        {
            let refused = request_shutdown(&stale).err();
            wait_until(SPAWN_DEADLINE, || !is_live(env));
            refused
        } else {
            None
        };

        stand_down_legacy_daemon(env);
        let child = spawn_child(env)?;
        await_healthy(env, child).map_err(|failure| match refusal {
            Some(refused) => failure.with_context(&format!(
                "the daemon already holding this store did not stand down: {refused}"
            )),
            None => failure,
        })
    })();

    publish_attempt(env, &outcome);
    let _ = FileExt::unlock(&lock);
    outcome
}

/// Stands down a daemon left over from a build that published its portfile at
/// the state home's root.
///
/// **The one real transition hazard in keying daemon state by store.** Before
/// the key existed there was one portfile per state home; after it, a client
/// looks under `daemons/<key>/`, finds nothing, and would otherwise start a
/// second daemon on a store the old one still has open. The old daemon holds the
/// *old* pidfile, so the new one's `claim_pidfile` succeeds and nothing stops
/// it — two daemons, two page caches, two change tokens, two backup schedules.
///
/// Only for the default store: no other store had a daemon before this existed.
/// Only when this store's own portfile is absent, so the check costs one
/// `exists` on every subsequent run and nothing else. Called under the spawn
/// lock, so two clients racing through an upgrade stand the old daemon down
/// once.
///
/// Liveness here is "does it still answer", not the pidfile lock: the daemon
/// being stood down is by definition one whose runtime files are not where this
/// build looks, and asking it directly is both simpler and the question that
/// actually matters. Best-effort throughout — a legacy daemon that cannot be
/// reached is one that is already gone.
fn stand_down_legacy_daemon(env: &Environment) {
    if !env.store().is_default() || env.daemon_file().exists() {
        return;
    }
    let legacy = env.legacy_daemon_file();
    let Some(info) = read_info_at(&legacy) else {
        return;
    };
    if hello(&info).is_err() {
        // Nothing is answering there; the portfile outlived its daemon.
        let _ = std::fs::remove_file(&legacy);
        return;
    }
    let _ = request_shutdown(&info);
    wait_until(SPAWN_DEADLINE, || hello(&info).is_err());
    let _ = std::fs::remove_file(&legacy);
}

/// Marks every descriptor above stderr close-on-exec, in the child, between
/// `fork` and `exec`.
///
/// **Marking rather than closing is deliberate.** `std` reports a failed `exec`
/// to the parent over a close-on-exec pipe of its own: the parent reads
/// end-of-file and concludes the child got as far as running. Closing that
/// descriptor here would turn "the daemon binary could not be executed" into
/// "the daemon started, then vanished", which surfaces five seconds later as a
/// timeout naming nothing. Setting a flag it already has costs nothing and
/// keeps the report.
///
/// Only `fcntl` is called, which is async-signal-safe, as code between `fork`
/// and `exec` in a multi-threaded process must be.
///
/// The upper bound is the process's own descriptor table, clamped: a soft
/// `RLIMIT_NOFILE` of a million would otherwise make this loop cost more than
/// the spawn it protects. Measured on the development machine, where
/// `getdtablesize()` answers 245,760: 32ms, against a daemon spawn already
/// measured in hundreds.
#[cfg(unix)]
fn disinherit_descriptors() {
    /// Beyond this, a descriptor is not something storyhook opened.
    const CEILING: libc::c_int = 262_144;

    // SAFETY: `getdtablesize` reads a process property and takes no arguments.
    let table = unsafe { libc::getdtablesize() }.clamp(3, CEILING);
    for fd in 3..table {
        // SAFETY: `fcntl` on a descriptor that is not open returns `EBADF`,
        // which is the answer for most of this range and is ignored on purpose.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

/// Starts a detached daemon process.
///
/// It inherits this process's environment, which is how a test harness's
/// isolated data and state homes reach it. `STORYHOOK_PARENT_PID` travels with
/// it too: a daemon started by a test binary must not outlive that binary, and
/// the daemon polls for its parent rather than trusting anybody to reap it.
///
/// It inherits **nothing else**, and that is enforced rather than assumed — see
/// [`disinherit_descriptors`] and `tests/daemon_fd_hygiene.rs`.
///
/// **The handle is returned rather than dropped**, and that is the whole point
/// of this signature. A dropped `Child` on Unix is not a detached process, it
/// is an unwatched one: the daemon still runs, but nobody can ask whether it is
/// still running. [`await_healthy`] needs to, because a daemon that exits
/// during startup is otherwise indistinguishable from one that is slow, and
/// the client spent the whole five-second deadline finding that out.
fn spawn_child(env: &Environment) -> Result<std::process::Child, AppError> {
    let (exe, _) = current_binary()?;
    std::fs::create_dir_all(env.daemon_state_dir())?;
    // A crash's whole stderr — the default panic output the hook in
    // `crate::daemon::crash` chains to, and everything printed before it —
    // lives only in this file, and this line about to `truncate(true)` it
    // used to destroy that evidence on the very next spawn, which is usually
    // the very next `story` command (SH-287). Rotating one deep is enough:
    // `crash::harvest`, called from `run` a few lines below this function's
    // caller, reads `daemon.log.1` before anything else can touch it.
    let _ = std::fs::rename(env.daemon_log(), env.daemon_log_rotated());
    // Mode 0600, not `File::create`'s 0644: this log carries the daemon's whole
    // stderr for its whole life, and since SH-153 that can include a GitHub
    // token surfaced in a diagnostic — every other daemon file that matters is
    // already 0600 (`publish_inflight`, the pidfile).
    let mut log_options = OpenOptions::new();
    log_options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut log_options, 0o600);
    let log = log_options
        .open(env.daemon_log())
        .map_err(|e| AppError::Storage(format!("failed to create the daemon log: {e}")))?;
    // Any failure still on disk belongs to a previous spawn. Removing it here —
    // in the one place that starts a daemon, immediately before starting it —
    // is what lets `read_startup_failure` treat whatever it finds afterwards as
    // this child's. A stale one would be worse than none: it would report an
    // old cause with total confidence.
    let _ = std::fs::remove_file(env.daemon_failure());

    // The port and the store travel on the argv rather than in the child's
    // environment. A client that was asked for a particular port — or a
    // particular store — holds that answer in its own `Environment`, and the
    // child builds a fresh one from the process environment, so passing either
    // implicitly would silently lose it. That is exactly what `story web start
    // --port N` did until this line existed, and for the store it would be
    // worse: a daemon published in *this* store's directory while holding a
    // different file is the disagreement the whole design exists to make
    // unrepresentable.
    let port = env.preferred_port().to_string();
    let store = env.store_path().to_path_buf();
    let mut command = Command::new(exe);
    command
        .arg("--store-path")
        .arg(&store)
        .args(["daemon", "--serve", "--port", &port])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    // Its own process group, so the daemon does not die with the terminal that
    // happened to start it — and so a test harness can kill the whole group.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    // The three descriptors above are the *only* ones the daemon may have.
    //
    // A daemon outlives the command that started it, so anything else it
    // inherits it holds forever — and a pipe's reader waits for the last writer
    // to close. That deadlock was captured in this repository's own gate: a
    // test binary blocked in `read(2)` for four minutes on a pipe whose write
    // end a `story … daemon --serve` was holding at fd 7, released in under
    // three seconds by killing the daemon. It cannot resolve on its own,
    // because the daemon stops when its parent does and its parent is the
    // process it has blocked (SH-94).
    //
    // Descriptors arrive by accident because `std` has no `pipe2` on macOS: it
    // creates the pipes behind `Stdio::piped()` inheritable and marks them
    // close-on-exec a syscall later, so a spawn from another thread inside that
    // window carries them off. The window is scheduling-wide on a busy machine,
    // which is why the symptom looked like load sensitivity.
    //
    // SAFETY: the closure runs after `fork` and before `exec`, where only
    // async-signal-safe calls are permitted. It makes one, `fcntl`.
    #[cfg(unix)]
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, || {
            disinherit_descriptors();
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|e| AppError::Storage(format!("failed to start the storyhook daemon: {e}")))
}

/// Polls until a daemon of this build is answering, or gives up loudly.
///
/// Two ways to stop waiting, and the second one is why `child` is a parameter:
///
/// 1. **A daemon answers.** The happy path.
/// 2. **The child exits.** A daemon that could not open the store, could not
///    bind, or was killed is *finished*, and no amount of further polling
///    changes that. Waiting the full deadline for it made every startup failure
///    cost five seconds and report a timeout, which describes the symptom and
///    not one thing about the cause.
///
/// The deadline survives for the case neither of those covers: a child that is
/// alive and has not published a portfile.
fn await_healthy(
    env: &Environment,
    mut child: std::process::Child,
) -> Result<DaemonInfo, AppError> {
    let deadline = Instant::now() + SPAWN_DEADLINE;
    while Instant::now() < deadline {
        if let Some(info) = read_info(env)
            && info.is_this_binary()
            && info.serves(env.store_path())
            && hello(&info).is_ok()
        {
            return Ok(info);
        }
        // Asked *after* the health check, never before: a daemon can publish
        // its portfile and be answering while this loop is between polls, and
        // a child that exited in that window still started successfully.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(daemon_exited(env, status));
        }
        std::thread::sleep(SPAWN_POLL);
    }
    Err(AppError::Storage(format!(
        "the storyhook daemon did not start within {}s, and is still running — so it is \
         stuck rather than broken.\n{}\nIts log is the first place to look.",
        SPAWN_DEADLINE.as_secs(),
        describe_paths(env),
    )))
}

/// The error for a daemon that started and then stopped before answering.
///
/// Prefers the daemon's *own* error, recorded by [`record_startup_failure`],
/// over anything this process could infer. A store that will not open is the
/// case that matters: the daemon computes a diagnosis naming the file, the
/// damage and the way back, and a client that reported "it exited with status
/// 5" instead would be discarding the only useful sentence available.
///
/// The exit description is kept either way. With a recorded failure it is
/// context; without one — a daemon killed by a signal, or one that died before
/// it could write — it is all there is, and it is still better than a timeout.
fn daemon_exited(env: &Environment, status: std::process::ExitStatus) -> AppError {
    let how = describe_exit(status);
    match read_startup_failure(env) {
        Some(error) => error.with_context(&format!(
            "the storyhook daemon could not start, so this command did not run. It {how}.\n{}",
            describe_paths(env),
        )),
        None => AppError::Storage(format!(
            "the storyhook daemon could not start: it {how}, without recording why.\n{}\nIts log \
             is the place to look.",
            describe_paths(env),
        )),
    }
}

/// Records why a daemon is about to stop, for the client that started it.
///
/// Written by the `--serve` process itself, on its way out, because it is the
/// only one that knows. Best-effort by design: a daemon that cannot write this
/// file has a worse problem than the report, and failing here would replace a
/// good diagnosis with a bad one about a diagnostic.
pub fn record_startup_failure(env: &Environment, error: &AppError) {
    let _ = std::fs::create_dir_all(env.daemon_state_dir());
    if let Ok(document) = serde_json::to_string(&crate::error::WireError::from(error)) {
        let _ = std::fs::write(env.daemon_failure(), document);
    }
}

/// The failure a previous `--serve` recorded, if this spawn left one.
///
/// [`spawn_child`] deletes any earlier file before starting, so anything read
/// here belongs to the child this client just started. Without that, a client
/// would attribute a months-old failure to today's daemon.
fn read_startup_failure(env: &Environment) -> Option<AppError> {
    let raw = std::fs::read_to_string(env.daemon_failure()).ok()?;
    serde_json::from_str::<crate::error::WireError>(&raw)
        .ok()
        .map(AppError::from)
}

/// How a process ended, in words rather than in a `Debug` rendering.
///
/// A signal is reported as a signal. `ExitStatus`'s own `Display` renders one
/// as `signal: 9 (SIGKILL)`, which is accurate and reads like a leaked
/// implementation detail in the middle of a sentence.
fn describe_exit(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return format!("was killed by signal {signal}");
    }
    match status.code() {
        Some(code) => format!("exited with status {code}"),
        None => "stopped for a reason the operating system did not report".to_string(),
    }
}

/// Blocks until `ready` answers true, or `deadline` elapses.
fn wait_until(deadline: Duration, ready: impl Fn() -> bool) -> bool {
    let until = Instant::now() + deadline;
    while Instant::now() < until {
        if ready() {
            return true;
        }
        std::thread::sleep(SPAWN_POLL);
    }
    ready()
}

/// How long a *control* request waits for the whole exchange.
///
/// These are the calls that carry no work: an identity check and a shutdown
/// request. Both are loopback round trips against a process that is either
/// healthy or has stopped being a daemon, so five seconds is enormous — and the
/// question a bound answers here is not "is it slow" but "is it ever coming
/// back".
///
/// Public so `the_daemon_deadline_family_is_ordered` can name it. The family it
/// belongs to is documented on [`SERVED_DEADLINE`].
pub const CONTROL_DEADLINE: Duration = Duration::from_secs(5);

/// How long a client waits for the daemon to finish **its own** request.
///
/// # This is a completion deadline, and it is not a liveness bound
///
/// Said plainly because the story that produced it asked for a liveness bound
/// and one does not exist here. The daemon writes no bytes before its handler
/// returns, so byte-idleness on the socket is *arithmetically identical* to
/// waiting for completion — zero bytes move whether the work is wedged or merely
/// long. What this bound changes is not the shape of the clock but **which
/// seconds it counts**: it is measured on the daemon's own published boundaries
/// ([`CurrentRequest`]) rather than on the client's wall clock.
///
/// The record's contribution is **subtraction, not detection**. An entry/exit
/// record cannot see inside a request, so it cannot tell "wedged serving mine"
/// from "still working on mine" — and nothing here pretends it can. What it
/// removes is *other people's work* from your wait, which is the only thing that
/// made a per-command number incoherent: the daemon serves one request at a
/// time, so a naked wall-clock deadline on `story list` would have to be as long
/// as the longest `pr-check` it might be queued behind, or it would abandon
/// healthy work (SH-144).
///
/// # Where the number comes from
///
/// **Calibrated, not derived, and that distinction is deliberate** — for the
/// *local-work* component this constant alone covers. `hooks.settings.timeout_seconds`
/// used to have no ceiling, which meant no honest derivation of the hook
/// component existed and a fake one would have survived a review it should
/// not have. SH-174 closed that gap: [`crate::event_hooks::HOOK_TIMEOUT_CEILING_SECS`]
/// (60s) now bounds every configured hook, so the *allowance*
/// [`served_deadline`] and [`served_deadline_for`] add on top of this
/// constant is provably bounded — ≤60s for a single hook, ≤120s for the
/// `set-state`/`set-fields` transition pair, ≤120s × N for `bulk-update` —
/// rather than the unknown this constant used to have to hedge against.
/// `SERVED_DEADLINE` itself stays a calibration rather than becoming a
/// derivation, because it answers a different question than the ceiling
/// does — how long storyhook's own local work can take with **no** hooks
/// configured at all — and that number was never a function of the hook
/// ceiling to begin with.
///
/// So: 120s is 26x the slowest local command ever measured here — a 8,000-story
/// import at 4.57s, against a store 33x the size of the real one — and 12x the
/// default event-hook timeout. A record that has not moved in two minutes is not
/// a daemon that is busy.
///
/// The family, and the one rule that orders it: what the wait is *over*.
///
/// | constant | the wait is over | justification |
/// |---|---|---|
/// | [`CONTROL_DEADLINE`] 5s | a round trip carrying no work | chosen — no legitimate slow case exists |
/// | `SPAWN_DEADLINE` 5s | a process coming up | chosen — bounded by the OS |
/// | [`SPAWN_LOCK_DEADLINE`] 30s | storyhook's own bounded code | derived — the sum of what the holder can spend |
/// | `SERVED_DEADLINE` 120s | the user's own data | calibrated — see above |
/// | [`GIT_HOOK_DEADLINE`] 10s | a git command a human is watching | chosen — git imposes no budget of its own |
pub const SERVED_DEADLINE: Duration = Duration::from_secs(120);

/// The same bound, for the one command whose duration is set off this machine.
///
/// `pr-check` loops once per open linked pull request against a client that
/// bounds each call at 30s, so its total is O(linked PRs) x 30s and cannot be
/// calibrated from local measurement at all — the same shape the retired
/// story↔GitHub-Issues sync engine's own O(stories x comments) x 30s total
/// had (SH-408), and the reason this constant is not renamed away from the
/// arithmetic that produced it. 3600s is **120 consecutive worst-case
/// calls**: a check in which that many linked pull requests in a row exhaust
/// their timeout is throttled or offline, and stopping is the right answer.
///
/// **A longer value, never no value.** An exemption was proposed and rejected:
/// it would restore the forever-hang this work exists to remove — including a
/// credential prompt wedged in a terminal-less `Select::interact()` (SH-153),
/// which would then never fire at all. One unbounded arm anywhere means the
/// story has not shipped.
pub const PR_CHECK_SERVED_DEADLINE: Duration = Duration::from_secs(3600);

/// When a client explains itself to a human who is still waiting.
///
/// `2 * SPAWN_DEADLINE`, and also exactly `HooksSettings::default()`'s
/// `timeout_seconds` — the first moment a wait can no longer be accounted for by
/// storyhook's own measured work or by one default-timeout event hook.
pub const SERVED_PATIENCE: Duration = Duration::from_secs(2 * SPAWN_DEADLINE.as_secs());

/// How often a waiting client re-reads the record.
///
/// Matches the two intervals the daemon already polls its own state on
/// (`CHANGE_TOKEN_POLL`, `SHUTDOWN_CHECK`). A short command pays none of it: the
/// polling thread defers its first read by one interval, so a command that
/// finishes in 50ms performs no extra file reads at all.
pub const RECORD_POLL: Duration = Duration::from_millis(250);

/// How long a client waits for a daemon that publishes **nothing at all**.
///
/// The branch that exists because the set can be empty for a reason no
/// entry can describe: the daemon accepted the connection and never reached
/// [`Entry::name`]. Post-SH-144 this is the only shape the unauthenticated
/// wedge can present to a client, and the reason it gets its own constant rather
/// than sharing [`SERVED_DEADLINE`] is that its message is different — a daemon
/// that is alive, holding its pidfile, and has not started your command.
pub const UNPUBLISHED_DEADLINE: Duration = Duration::from_secs(120);

/// The `--deadline` a managed git hook gives its own `story` calls (SH-343).
///
/// # Not derived — chosen, and the family table above says so honestly
///
/// A tempting shape was tried and rejected: summing [`SPAWN_DEADLINE`] and
/// [`CONTROL_DEADLINE`] to make this *look* derived. It is not. [`CONTROL_DEADLINE`]
/// is documented above as bounding "a round trip carrying no work" — `hello`,
/// `request_shutdown` — and a git hook's `story` call carries work. The honest
/// sum for what a hook actually waits on is [`SPAWN_DEADLINE`] +
/// [`SERVED_DEADLINE`] (125s), which is exactly the unbounded-feeling wait this
/// story exists to remove. No derivation from this family answers "how long may
/// a git command a human is watching legitimately block", because git supplies
/// no budget the way `hooks.json` does for the Claude Code plugin's own
/// `--deadline` precedents (`session-start.sh`, `post-git.sh`, `stop-handoff.sh`
/// — each sized against a *manifest* timeout that has no counterpart here).
///
/// So this is a **judgement call about a human's patience with `git commit`**,
/// pinned at the low end, with two facts that bound it rather than derive it:
///
/// - **Above [`SPAWN_DEADLINE`] (5s).** Traced: on an uncontended cold start,
///   `lifecycle::ensure` → `spawn_locked` takes a free lock instantly,
///   `stand_down_legacy_daemon` returns at once with nothing live, and the
///   stale-daemon `wait_until` is skipped — so the only real wait is
///   [`await_healthy`], already capped at `SPAWN_DEADLINE`. A value at or below
///   it would abandon a cold start `await_healthy` itself would have accepted,
///   which stops the sync firing after every reboot — silently, since these
///   hooks must stay quiet ([`tests/hook_silence.rs`]).
/// - **Below [`SPAWN_LOCK_DEADLINE`] (30s).** So *contention* — another client
///   mid-spawn — is abandoned rather than queued. That is correct here and
///   nowhere else in this family: the client we would queue behind leaves a
///   warm daemon behind for the next hook, and every managed hook's own answer
///   is discarded (`--quiet 2>/dev/null || true`) except `prepare-commit-msg`'s,
///   whose degrade-on-give-up ("no story hint appended") is the sanctioned
///   shape SH-182 already established.
///
/// `commit-sync` fires no event hooks by design (`Store::commit_sync`'s own
/// doc), so [`crate::event_hooks::HOOK_TIMEOUT_CEILING_SECS`] does not bear on
/// three of the four call sites this bounds. Only `story move`'s
/// `on_state_change`/`on_close` pair can legitimately exceed this deadline —
/// abandoning that wait is harmless, since expiry does not cancel the request
/// and the daemon completes the move regardless; nothing reads the answer.
///
/// **Cross-file invariant this value must keep**: `post-git.sh` skips its own
/// sync when a managed `post-commit` exists, crediting that hook for work it
/// may not finish — so `post-git.sh`'s own `--deadline` must stay `<=` this
/// constant, or the skip loses coverage it currently has. Pinned in
/// `tests/hook_budgets.rs`.
///
/// Do not alias [`SERVED_PATIENCE`], which is also 10s today. It answers a
/// different question — "when does a client explain itself to a human" — and
/// coupling the two means moving one silently moves the other, the same
/// hazard `SERVED_DEADLINE`'s own doc already refused for
/// [`UNPUBLISHED_DEADLINE`].
pub const GIT_HOOK_DEADLINE: Duration = Duration::from_secs(10);

/// How long a client will wait for the daemon to finish one command.
///
/// A type rather than a bare `Duration` so that "no bound" is a value somebody
/// chose rather than a `None` that reads as "unset". Nothing in storyhook's
/// **production** code constructs either variant — every command is bounded
/// by [`SERVED_DEADLINE`] or [`PR_CHECK_SERVED_DEADLINE`], both of which
/// [`crate::event_hooks::HOOK_TIMEOUT_CEILING_SECS`] now makes provably
/// sufficient (SH-174). This type's sole former producer,
/// `$STORYHOOK_EXCHANGE_DEADLINE_SECS`, is gone with the gap it existed to
/// paper over. What remains is a pure test seam: [`verdict`]'s
/// `override_bound` parameter and [`crate::invoke::HttpInvoker::send`]'s
/// `bound` parameter let a test drive a 120-second bound in 250 milliseconds
/// without a real sleep, while [`crate::invoke::HttpInvoker::invoke`] passes
/// `None` unconditionally in production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeBound {
    /// Give up when the record has not moved for this long.
    After(Duration),
    /// Wait forever, because the user asked for the pre-SH-144 behaviour.
    Unbounded,
}

/// The deadline this daemon commits to for `command`, computed once by
/// whoever names an [`Entry`] and published as that record's
/// `served_deadline_secs` — so every client, including one only queued
/// behind somebody else's record, reads the same number the daemon derived,
/// rather than recomputing a guess it has no way to make correctly for a
/// project it cannot see.
///
/// `pr-check`'s total is set off this machine (SH-144) and gets its own
/// constant regardless of hook configuration — its duration is
/// O(linked pull requests) × a 30s per-call client bound, not a function of
/// any one project's hooks. `set-state` and `set-fields` widen by
/// [`crate::event_hooks::transition_pair_timeout`] instead of the general
/// case below, because they are the one pair of commands that can fire two
/// hooks serially in one call (SH-174) — every other command's deadline
/// widens by [`crate::event_hooks::max_configured_timeout`] at `cwd`: a
/// command's own completion time is bounded above by *some* configured
/// hook's timeout, so ignoring hook configuration entirely is what turned a
/// legitimately slow hook into an abandoned command — the gap
/// [`SERVED_DEADLINE`]'s own docstring names as the reason it could only be
/// calibrated, not derived.
///
/// `bulk-update`'s own O(n) multiplier is not handled here — see
/// [`served_deadline_for`], which is what the daemon actually calls.
#[must_use]
pub fn served_deadline(command: &str, cwd: &Path) -> Duration {
    // `pr-check`'s cost is O(linked pull requests) × a per-call client
    // bound, not a function of any one project's hooks — set off this
    // machine (SH-144), the same reason `PR_CHECK_SERVED_DEADLINE` is its
    // own constant rather than derived from hook configuration.
    if command == "pr-check" {
        return PR_CHECK_SERVED_DEADLINE;
    }
    let allowance = if command == "set-state" || command == "set-fields" {
        crate::event_hooks::transition_pair_timeout(cwd)
    } else {
        crate::event_hooks::max_configured_timeout(cwd)
    }
    .unwrap_or(Duration::ZERO);
    SERVED_DEADLINE + allowance
}

/// [`served_deadline`], widened further for a command whose cost scales with
/// how many items it touches.
///
/// `bulk-update` (SH-174) loops [`crate::service::StoryService::set_state`]
/// once per `(id, state)` pair it is given, so its worst-case hook time is
/// `updates.len() ×` [`crate::event_hooks::transition_pair_timeout`], not one
/// widening's worth of it — [`served_deadline`] alone would under-count by a
/// factor of `updates.len()`. A separate function rather than a parameter
/// [`served_deadline`] always takes: every other invocation's cost is one
/// command, one hook chain, so threading an item count through call sites
/// that have nothing to multiply would be a parameter nobody but this one
/// caller sets to anything but `1`.
///
/// The one production caller ([`crate::api::rpc`]) already holds the parsed
/// [`crate::cli::Invocation`] it can read `updates.len()` from — the number
/// is not guessed, it is the exact list the command is about to attempt.
///
/// `story claim` (SH-476) gets the same [`transition_pair_timeout`] widening
/// as `set-state`/`set-fields` above, for the identical reason: a claim is a
/// `StoryStateChanged` transition wearing a different verb, so it fires the
/// same `on_state_change`/`on_close` pair those two commands do. Without it, a
/// claim whose transition hooks run gets the ordinary deadline and times out
/// intermittently with nothing saying why. `story next` is a pure read since
/// SH-477 and keeps the general allowance below, and so does `story claim
/// --dry-run`, which fires no hook because it writes nothing.
///
/// [`transition_pair_timeout`]: crate::event_hooks::transition_pair_timeout
#[must_use]
pub fn served_deadline_for(invocation: &crate::cli::Invocation, cwd: &Path) -> Duration {
    if let crate::cli::Invocation::BulkUpdate { updates } = invocation {
        let per_item = crate::event_hooks::transition_pair_timeout(cwd).unwrap_or(Duration::ZERO);
        let n = u32::try_from(updates.len().max(1)).unwrap_or(u32::MAX);
        return SERVED_DEADLINE + per_item * n;
    }
    if matches!(
        invocation,
        crate::cli::Invocation::Claim { dry_run: false, .. }
    ) {
        let allowance = crate::event_hooks::transition_pair_timeout(cwd).unwrap_or(Duration::ZERO);
        return SERVED_DEADLINE + allowance;
    }
    served_deadline(crate::invoke::invocation_name(invocation), cwd)
}

/// What a waiting client has observed about the daemon, at the moment it
/// asks [`verdict`] what to do.
///
/// Two different clocks, because only one of them is ever a deadline.
/// `mine_for` — how long *my own* entry has been present, if it is — is set
/// the instant a client first sees its own `request_id` in the set and never
/// reset while that entry stays present; `since_change` is reset whenever
/// `inflight` changes at all, which is the signal a client with no entry of
/// its own — queued, or the daemon has published nothing — has to go on.
pub struct Observed<'a> {
    /// This client's own request id.
    pub mine: &'a str,
    /// Everything the daemon says it is serving, in arrival order.
    pub inflight: &'a [CurrentRequest],
    /// How long my own entry has looked exactly as it looks now, if
    /// `inflight` contains one naming `mine`.
    pub mine_for: Option<Duration>,
    /// How long the whole set has looked exactly as it looks now.
    pub since_change: Duration,
    /// The whole wait, from the first byte sent.
    pub total: Duration,
}

/// What a waiting client should do, given what it has seen.
///
/// **A pure function on purpose.** Every state this design can reach is decided
/// here, so the coverage is a table of unit tests that cost no wall clock at
/// all — including the one that matters most, "a client queued behind moving
/// work waits arbitrarily long", which any wall-clock design fails and which
/// would otherwise take a test minutes to express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The daemon is finishing things, or has not gotten to mine yet. Keep
    /// waiting, however long that takes.
    Wait,
    /// My own entry has been present past its own deadline.
    GiveUpMine(CurrentRequest),
    /// I am not in the set, and nothing in it has moved for the most patient
    /// deadline among the commands it names.
    GiveUpQueued(Vec<CurrentRequest>),
    /// The daemon published nothing at all for [`UNPUBLISHED_DEADLINE`].
    GaveUpUnpublished,
}

/// Decides whether to keep waiting.
///
/// Note what is missing from the "queued" branch: there is no clock on
/// queued time itself, and that is the design rather than an omission. While
/// the set keeps changing the daemon is finishing work, so a client behind a
/// long queue waits as long as the queue takes — that is SH-144's own M9
/// false positive, deleted rather than tuned, and it survives this rewrite
/// unchanged. `override_bound` is exactly what its name says: `None` means
/// *use the deadline each record already carries*, which is the production
/// path and the whole point of publishing it there —
/// [`crate::invoke::HttpInvoker::invoke`] passes `None` unconditionally.
/// `Some` exists only in tests since SH-174 deleted its production producer:
/// it replaces every deadline below — including the unpublished one — and is
/// how a test drives a 120-second bound in 250 milliseconds without a real
/// sleep.
#[must_use]
pub fn verdict(seen: &Observed<'_>, override_bound: Option<ExchangeBound>) -> Verdict {
    if let Some(ExchangeBound::Unbounded) = override_bound {
        return Verdict::Wait;
    }
    let deadline_of = |record: &CurrentRequest| match override_bound {
        Some(ExchangeBound::After(d)) => d,
        _ => Duration::from_secs(record.served_deadline_secs),
    };

    // My own entry, if the set carries one: bounded by its own deadline,
    // whatever else is or is not also in flight.
    let mine = seen.inflight.iter().find(|r| r.request_id == seen.mine);
    if let (Some(mine_for), Some(record)) = (seen.mine_for, mine) {
        return if mine_for >= deadline_of(record) {
            Verdict::GiveUpMine(record.clone())
        } else {
            Verdict::Wait
        };
    }

    // Not mine, or not there yet: queued behind whatever is in flight. A
    // record frozen on somebody else's request still means the daemon has
    // stopped finishing things, and it still blocks me — bounded by the most
    // patient deadline any of them carries, because I am behind all of them.
    if !seen.inflight.is_empty() {
        let patience = seen
            .inflight
            .iter()
            .map(deadline_of)
            .max()
            .unwrap_or(SERVED_DEADLINE);
        return if seen.since_change >= patience {
            Verdict::GiveUpQueued(seen.inflight.to_vec())
        } else {
            Verdict::Wait
        };
    }

    // The set is empty. Either the daemon is between requests — in which
    // case one is about to appear and `since_change` keeps resetting — or it
    // never published at all, which is the branch below.
    let unpublished = match override_bound {
        Some(ExchangeBound::After(d)) => d,
        _ => UNPUBLISHED_DEADLINE,
    };
    if seen.total >= unpublished {
        Verdict::GaveUpUnpublished
    } else {
        Verdict::Wait
    }
}

/// The client storyhook talks to daemons with.
///
/// **Every call to a daemon needs a deadline, and none of them had one.** The
/// tempting assumption is that loopback either answers or refuses; a process
/// that accepts a connection and then never writes does neither, and holds its
/// peer indefinitely. Reachable three ways, all of them real: a daemon wedged on
/// a long operation, a daemon stuck in a probe — W0 found `tailscale status`
/// hanging for minutes and leaving servers bound and silent — and, the case
/// [`hello`] exists for, something that is not storyhook at all holding the
/// port.
///
/// The cost of not having one was a `story daemon stop` that never returned and
/// never said why, and it was measured: W8's concurrency soak stalled a `make
/// test` run for twelve minutes inside a `DaemonGuard`'s teardown.
///
/// `timeout_global` rather than a connect timeout, because for these two calls
/// the *answer* is the point and there is no legitimate slow case. The invoker's
/// own request is bounded differently, and says why there.
fn control_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(CONTROL_DEADLINE))
        .build()
        .into()
}

/// Asks a daemon who it is.
///
/// Checked before a client trusts a daemon it discovered rather than started: a
/// port is a number anybody can hold, and a portfile can outlive the process
/// that wrote it. The answer has to agree with the portfile about the version
/// and the pid, or the thing on that port is somebody else.
pub fn hello(info: &DaemonInfo) -> Result<(), AppError> {
    let url = format!("http://127.0.0.1:{}/api/v1/hello", info.port);
    let response = control_agent()
        .get(&url)
        .header("X-Storyhook-Token", &info.token)
        .call()
        .map_err(|e| AppError::Storage(format!("the daemon did not answer /api/v1/hello: {e}")))?;
    let body: Hello = response
        .into_body()
        .read_json()
        .map_err(|e| AppError::Storage(format!("the daemon's identity was unreadable: {e}")))?;
    if body.version != info.version || body.pid != info.pid {
        return Err(AppError::Storage(format!(
            "the service on port {} is not this daemon (it reports storyhook {} pid {}, \
             the portfile says {} pid {})",
            info.port, body.version, body.pid, info.version, info.pid
        )));
    }
    Ok(())
}

/// Asks a daemon to arm a one-shot handoff coupon (SH-251).
///
/// The credential half of `story web open`: the coupon travels in the URL
/// fragment the browser is opened at, and the page spends it for the real
/// token. Nothing in this exchange is durable — see
/// [`crate::api::handoff`] for what that buys and what it does not.
///
/// Loopback only, and authenticated with the portfile's token, because
/// [`crate::api::rpc::admission`] owns `/api/v1/*`. A caller that could not
/// read the portfile could not call this, and a caller that *can* read the
/// portfile already has the token this coupon would buy.
pub fn arm_handoff(info: &DaemonInfo) -> Result<String, AppError> {
    let url = format!(
        "http://127.0.0.1:{}{}",
        info.port,
        crate::api::handoff::ARM_PATH
    );
    let response = control_agent()
        .post(&url)
        .header(crate::api::rpc::TOKEN_HEADER, &info.token)
        .send_empty()
        .map_err(|e| AppError::Storage(format!("the daemon did not arm a handoff: {e}")))?;
    let armed: ArmedHandoff = response
        .into_body()
        .read_json()
        .map_err(|e| AppError::Storage(format!("the daemon's handoff was unreadable: {e}")))?;
    Ok(armed.coupon)
}

/// [`arm_handoff`]'s answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ArmedHandoff {
    /// The coupon, good for one redemption within
    /// [`crate::api::handoff::TTL`].
    coupon: String,
}

/// `/api/v1/hello`'s answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    /// The storyhook version the daemon was built from.
    pub version: String,
    /// The protocol it speaks.
    pub protocol: u32,
    /// Its process id.
    pub pid: u32,
    /// When it started.
    pub started_at: String,
}

/// Asks a daemon to mint a fresh named token (SH-255): `story token new`.
///
/// Loopback and the portfile's token, like [`arm_handoff`] and
/// [`revoke_named_token`] — `/api/v1/*` is [`crate::api::rpc::admission`]'s,
/// and minting the credential that will itself authenticate the dashboard is
/// an administrative act, not something a browser tab may ask for on its
/// own.
pub fn mint_named_token(info: &DaemonInfo, name: &str) -> Result<MintedNamedToken, AppError> {
    let url = format!(
        "http://127.0.0.1:{}{}?name={name}",
        info.port,
        crate::api::tokens::TOKENS_PATH,
    );
    let response = control_agent()
        .post(&url)
        .header(crate::api::rpc::TOKEN_HEADER, &info.token)
        .config()
        .http_status_as_error(false)
        .build()
        .send_empty()
        .map_err(|e| AppError::Storage(format!("the daemon did not answer: {e}")))?;
    read_token_route_reply(response)
}

/// Asks a daemon for every live named token: `story token list`. Never a
/// secret — [`crate::api::tokens::TokenRegistry::list`] cannot produce one
/// after mint time, so there is nothing this route could leak beyond what
/// the registry itself already lacks.
pub fn list_named_tokens(
    info: &DaemonInfo,
) -> Result<Vec<crate::api::tokens::TokenSummary>, AppError> {
    let url = format!(
        "http://127.0.0.1:{}{}",
        info.port,
        crate::api::tokens::TOKENS_PATH
    );
    let response = control_agent()
        .get(&url)
        .header(crate::api::rpc::TOKEN_HEADER, &info.token)
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|e| AppError::Storage(format!("the daemon did not answer: {e}")))?;
    let listed: ListedNamedTokens = read_token_route_reply(response)?;
    Ok(listed.tokens)
}

/// Asks a daemon to end one named token immediately, whether or not it has
/// expired: `story token revoke`.
pub fn revoke_named_token(info: &DaemonInfo, name: &str) -> Result<(), AppError> {
    let url = format!(
        "http://127.0.0.1:{}{}/{name}",
        info.port,
        crate::api::tokens::TOKENS_PATH,
    );
    let response = control_agent()
        .delete(&url)
        .header(crate::api::rpc::TOKEN_HEADER, &info.token)
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|e| AppError::Storage(format!("the daemon did not answer: {e}")))?;
    read_token_route_reply::<serde_json::Value>(response).map(|_| ())
}

/// Reads a `/api/v1/tokens` response: a 200 deserializes as `T`; anything
/// else becomes an [`AppError`] carrying the daemon's own text explanation —
/// a duplicate name, the registry at capacity, or an unknown name to revoke.
///
/// Every caller above asks for `http_status_as_error(false)` for exactly
/// this reason: the default would collapse a 409 and a 404 alike into one
/// opaque transport error and discard the body that says which happened,
/// where the daemon has already written the human-readable answer.
///
/// The status maps to the [`AppError`] variant a caller of a `story token`
/// command actually wants: 404 is [`AppError::NotFound`] (exit 3), 409 is
/// [`AppError::Validation`] (exit 2, a duplicate name or the registry at
/// capacity — the caller's input, not a broken daemon), 400 is
/// [`AppError::Usage`] (exit 2, a malformed request this CLI should not have
/// sent). Anything else is [`AppError::Storage`] (exit 5): genuinely
/// unexpected, since nothing about a well-formed `story token` invocation
/// should produce it.
fn read_token_route_reply<T: serde::de::DeserializeOwned>(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<T, AppError> {
    let status = response.status().as_u16();
    if status != 200 {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        let message = if body.is_empty() {
            format!("the daemon answered with status {status}")
        } else {
            body
        };
        return Err(match status {
            404 => AppError::NotFound(message),
            409 => AppError::Validation(message),
            400 => AppError::Usage(message),
            _ => AppError::Storage(message),
        });
    }
    response
        .into_body()
        .read_json()
        .map_err(|e| AppError::Storage(format!("the daemon's answer was unreadable: {e}")))
}

/// [`mint_named_token`]'s answer — the raw secret, returned exactly once.
/// Nothing the daemon holds afterward can reproduce it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MintedNamedToken {
    pub token: String,
    pub name: String,
    pub prefix: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// [`list_named_tokens`]'s answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ListedNamedTokens {
    tokens: Vec<crate::api::tokens::TokenSummary>,
}

/// Asks a daemon to shut down. Returns as soon as the request is accepted —
/// it does not wait for the daemon to actually exit.
///
/// The daemon answers a shutdown request *before* it drains and exits
/// (`serve.rs`'s `Verdict::Shutdown` handling), so `Ok(())` here means
/// "accepted," never "gone." Every caller that needs "gone" waits for it
/// separately, over [`is_live`]: [`stop`] (`wait_forever`/`wait_until`) and
/// [`spawn_locked`]'s and [`stand_down_legacy_daemon`]'s own bounded
/// `wait_until` calls. This function was previously documented as waiting,
/// which it never did (SH-345).
pub fn request_shutdown(info: &DaemonInfo) -> Result<(), AppError> {
    let url = format!("http://127.0.0.1:{}/api/v1/shutdown", info.port);
    control_agent()
        .post(&url)
        .header("X-Storyhook-Token", &info.token)
        .send_empty()
        .map_err(|e| AppError::Storage(format!("the daemon refused to shut down: {e}")))?;
    Ok(())
}

/// How `stop` behaves once it has asked the daemon to shut down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// Wait for the daemon to drain and exit on its own, however long that
    /// takes. The default — `story daemon stop` with no flag — because
    /// nothing is abandoned this way.
    Graceful,
    /// Give the daemon [`FORCE_GRACE`] to drain and exit on its own; past
    /// that, signal its pid directly. Whatever the daemon was still serving
    /// at that moment is abandoned — naming it is the caller's problem
    /// (`story daemon stop --force`'s), not this function's.
    Force,
}

/// How long [`StopMode::Force`] waits for an orderly exit before signalling.
///
/// Short, deliberately: forcing is the caller saying they will not wait, and
/// a daemon with nothing left in flight exits within one `SHUTDOWN_CHECK`
/// (250ms, `crate::daemon::serve`) of answering the shutdown request — this
/// is headroom for a handful of requests to finish, not a real drain window.
pub const FORCE_GRACE: Duration = Duration::from_secs(2);

/// Stops the running daemon, if there is one.
///
/// Reports what it stopped, or `None` when nothing was running — a distinction
/// the caller renders, because "already stopped" is a success for `daemon stop`
/// and a surprise for a restart.
pub fn stop(env: &Environment, mode: StopMode) -> Result<Option<DaemonInfo>, AppError> {
    if !is_live(env) {
        // Nothing holds the lock. Harvest whatever evidence the crash left
        // (SH-287) before clearing the portfile that names it — reading it
        // after the file is gone reads nothing. Then clear it, so `daemon
        // status` stops describing a process that is gone.
        crate::daemon::crash::harvest(env);
        let _ = std::fs::remove_file(env.daemon_file());
        return Ok(None);
    }
    let Some(info) = read_info(env) else {
        return Err(AppError::Storage(format!(
            "a daemon holds {} but published no portfile, so there is no way to \
             ask it to stop. Its pid is not knowable from here; kill it by hand.",
            env.daemon_pidfile().display()
        )));
    };
    request_shutdown(&info)?;
    match mode {
        StopMode::Graceful => wait_forever(env, &info),
        StopMode::Force => {
            if !wait_until(FORCE_GRACE, || !is_live(env)) {
                // Read before the signal, not after: once the daemon is dead
                // this file is whatever it last wrote, which is exactly the
                // set that never got to answer for itself.
                let abandoned = read_inflight(env);
                kill_pid(info.pid);
                if !wait_until(SPAWN_DEADLINE, || !is_live(env)) {
                    return Err(AppError::Storage(format!(
                        "signalled pid {} and it still holds {} after {}s; it may be \
                         unkillable (a zombie, or blocked in an uninterruptible wait)",
                        info.pid,
                        env.daemon_pidfile().display(),
                        SPAWN_DEADLINE.as_secs()
                    )));
                }
                ledger_abandoned(
                    env,
                    &abandoned,
                    "killed by `story daemon stop --force` after it did not drain \
                     within the grace period",
                );
            }
        }
    }
    let _ = std::fs::remove_file(env.daemon_file());
    Ok(Some(info))
}

/// Waits for a daemon to release its pidfile with no deadline of its own —
/// `story daemon stop --force` is the escape hatch, not a timeout buried in
/// here. Announces itself once, past [`SERVED_PATIENCE`], so a human at a
/// terminal learns the escape hatch exists rather than watching a command
/// that appears to hang.
fn wait_forever(env: &Environment, info: &DaemonInfo) {
    use std::io::IsTerminal;
    let started = Instant::now();
    let mut announced = false;
    while is_live(env) {
        if !announced && started.elapsed() >= SERVED_PATIENCE && std::io::stderr().is_terminal() {
            announced = true;
            eprintln!(
                "storyhook: waiting for the daemon (pid {}) to finish what it is running \
                 before it stops. `story daemon stop --force` gives it {}s more, then \
                 signals it directly.",
                info.pid,
                FORCE_GRACE.as_secs()
            );
        }
        std::thread::sleep(SPAWN_POLL);
    }
}

/// Sends `pid` an unignorable kill signal. Best-effort: a pid that has
/// already exited is not an error here, it is the outcome being asked for.
#[cfg(unix)]
fn kill_pid(pid: u32) {
    // SAFETY: `kill` with a real signal affects only the target process's own
    // lifecycle, and this process holds no lock or resource on its behalf.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_pid(_pid: u32) {}

/// Removes a portfile. Used by a daemon on its way out, best-effort.
pub fn clear_info(env: &Environment) {
    let _ = std::fs::remove_file(env.daemon_file());
}

/// The parent process a daemon must not outlive, if one was named.
///
/// A test binary sets `STORYHOOK_PARENT_PID` to its own pid before running
/// anything. Every `story` it spawns inherits the variable, so the daemon one of
/// them starts inherits it too — and polls for that pid, exiting when it goes
/// away. Four layers of orphan defence exist because three of them have failed:
/// a suite that leaks a daemon poisons every later run on the machine.
pub fn parent_pid() -> Option<u32> {
    std::env::var("STORYHOOK_PARENT_PID")
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
}

/// Whether `pid` is still a live process.
#[cfg(unix)]
pub fn pid_is_live(pid: u32) -> bool {
    // SAFETY: signal 0 performs error checking only — it delivers nothing and
    // cannot affect the target process.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
pub fn pid_is_live(_pid: u32) -> bool {
    true
}

/// Where the pidfile and portfile live, for diagnostics.
pub fn describe_paths(env: &Environment) -> String {
    format!(
        "store    {}\nportfile {}\npidfile  {}\nlog      {}",
        env.store_path().display(),
        env.daemon_file().display(),
        env.daemon_pidfile().display(),
        env.daemon_log().display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Environment;

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-lifecycle-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    /// What [`super::super::serve::bind_listeners`] returns on a machine with
    /// no tailnet: loopback bound, nothing else.
    fn loopback_only(port: u16) -> super::super::serve::BoundAddress {
        super::super::serve::BoundAddress {
            loopback: SocketAddr::from(([127, 0, 0, 1], port)),
            tailnet: None,
        }
    }

    /// A daemon advertises what it bound and nothing else. Pure — no network,
    /// no `tailscale`, no machine state — so it runs and can fail on every
    /// machine, which is the floor under the shim tests in
    /// `tests/tailnet_advertise.rs` and the real-tailnet ones in `web_test.rs`.
    #[test]
    fn the_advertised_host_is_the_bound_one_and_loopback_otherwise() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let build = |bound: &super::super::serve::BoundAddress| {
            info_for(
                bound,
                "t".to_string(),
                "2026-01-01T00:00:00Z",
                env.store_path(),
            )
            .expect("building the info")
        };

        let loopback = build(&loopback_only(4321));
        assert_eq!(loopback.advertised_host(), "127.0.0.1");
        assert_eq!(loopback.dashboard_url(), "http://127.0.0.1:4321");

        let mut with_tailnet = loopback_only(4321);
        with_tailnet.tailnet = Some(
            crate::daemon::tailnet::TailnetIdentity {
                bind_ip: "100.71.206.33".parse().expect("a fixture IPv4"),
                magic_dns: Some("psamathe.tail983f02.ts.net".to_string()),
            }
            .into_bound(),
        );
        let advertised = build(&with_tailnet);
        assert_eq!(advertised.advertised_host(), "psamathe.tail983f02.ts.net");
        assert_eq!(
            advertised.dashboard_url(),
            "http://psamathe.tail983f02.ts.net:4321"
        );
    }

    /// A portfile written before the `tailnet` field existed must still parse.
    ///
    /// [`read_info_at`] treats a portfile it cannot parse as *absent*, and an
    /// absent portfile beside a held pidfile is the one state `stop` cannot
    /// resolve — it can only tell the user to kill the process by hand. So the
    /// serde default is load-bearing rather than tidy. It reads as loopback,
    /// which is always true: loopback is bound unconditionally.
    #[test]
    fn a_portfile_without_a_tailnet_field_reads_as_loopback_only() {
        let dir = scratch();
        let path = dir.path().join("daemon.json");
        std::fs::write(
            &path,
            r#"{"pid":1,"port":4321,"version":"0.0.0","protocol":1,"exe":"/bin/story",
               "exe_mtime":0,"started_at":"2026-01-01T00:00:00Z","token":"t",
               "store_path":"/tmp/store.db"}"#,
        )
        .expect("writing a portfile from before the field existed");

        let info = read_info_at(&path).expect("an older portfile still parses");
        assert_eq!(info.tailnet, None);
        assert_eq!(info.advertised_host(), "127.0.0.1");
    }

    /// Disinheriting must not swallow the report that `exec` failed.
    ///
    /// This is the only assertion that distinguishes [`disinherit_descriptors`]'s
    /// two available implementations, and it is the reason for the one it has.
    /// `std` tells the parent that `exec` failed by writing an errno to a
    /// close-on-exec pipe; the parent reads end-of-file and concludes the child
    /// got as far as running. **Marking** that descriptor close-on-exec — which
    /// it already is — leaves the report intact. **Closing** it makes a failed
    /// `exec` indistinguishable from a successful one, so `spawn` returns `Ok`,
    /// `await_healthy` polls a daemon that never existed, and five seconds
    /// later the user is told it "did not start" with no reason given.
    ///
    /// Change the `fcntl` to a `close` and this test fails; nothing else in the
    /// suite does.
    #[cfg(unix)]
    #[test]
    fn disinheriting_still_reports_an_exec_that_failed() {
        let mut command = Command::new("/nonexistent/storyhook-is-not-here");
        // SAFETY: the closure calls only `fcntl` and `getdtablesize`, as
        // required between `fork` and `exec`.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut command, || {
                disinherit_descriptors();
                Ok(())
            });
        }
        let error = command
            .spawn()
            .expect_err("spawning a path that does not exist must fail");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::NotFound,
            "the failure must name the missing executable, not arrive later as \
             a daemon that never answered: {error}"
        );
    }

    #[test]
    fn a_portfile_round_trips_through_disk() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let info = info_for(
            &loopback_only(4321),
            "deadbeef".to_string(),
            "2026-01-01T00:00:00Z",
            env.store_path(),
        )
        .expect("building the info");
        write_info(&env, &info).expect("writing the portfile");
        assert_eq!(read_info(&env), Some(info));
    }

    /// The portfile carries a bearer token for an API that can do anything the
    /// CLI can. A world-readable one is a local privilege escalation.
    #[cfg(unix)]
    #[test]
    fn the_portfile_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let env = Environment::at(dir.path());
        let info = info_for(
            &loopback_only(4321),
            "deadbeef".to_string(),
            "2026-01-01T00:00:00Z",
            env.store_path(),
        )
        .expect("building the info");
        write_info(&env, &info).expect("writing the portfile");
        let mode = std::fs::metadata(env.daemon_file())
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    #[test]
    fn a_portfile_this_build_cannot_parse_reads_as_absent() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
        std::fs::write(env.daemon_file(), "{\"port\": \"not a number\"}").unwrap();
        assert_eq!(
            read_info(&env),
            None,
            "an unparseable portfile is another version's, and the right response \
             is to start a daemon of our own rather than to fail"
        );
    }

    #[test]
    fn no_daemon_means_the_pidfile_lock_is_free() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert!(!is_live(&env));
    }

    /// Liveness is the held lock, so it must answer true exactly while a daemon
    /// holds it — including for a daemon that never got to write a portfile.
    #[test]
    fn a_held_pidfile_reads_as_live_and_a_released_one_does_not() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let held = claim_pidfile(&env).expect("claiming the pidfile");
        assert!(is_live(&env));
        drop(held);
        assert!(!is_live(&env));
    }

    #[test]
    fn a_second_daemon_cannot_claim_a_held_pidfile() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let _held = claim_pidfile(&env).expect("claiming the pidfile");
        let second = claim_pidfile(&env);
        assert!(
            second.is_err(),
            "two daemons on one store is the state the lock exists to prevent"
        );
    }

    /// The keyed directory is what keeps two stores apart, and this is the
    /// belt to its braces: a portfile naming a different store means two
    /// canonical paths collided on one digest, and "close enough" there is
    /// SH-123 again.
    #[test]
    fn a_daemon_naming_another_store_is_not_usable() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let info = info_for(
            &loopback_only(4321),
            "t".to_string(),
            "2026-01-01T00:00:00Z",
            &dir.path().join("somebody-elses.db"),
        )
        .expect("building the info");
        write_info(&env, &info).expect("writing the portfile");
        let _held = claim_pidfile(&env).expect("claiming the pidfile");

        assert!(
            info.is_this_binary(),
            "the fixture must isolate one variable"
        );
        assert!(is_live(&env), "the fixture must isolate one variable");
        assert!(
            !info.serves(env.store_path()),
            "the fixture must name a different store"
        );
        assert!(
            usable(&env).is_none(),
            "a daemon holding another store must never be reused"
        );
    }

    #[test]
    fn a_daemon_from_another_build_is_not_this_binary() {
        let info = DaemonInfo {
            pid: 1,
            port: 1,
            version: "0.0.0-not-this-one".to_string(),
            protocol: PROTOCOL,
            exe: PathBuf::from("/nowhere/story"),
            exe_mtime: 0,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            token: "t".to_string(),
            store_path: PathBuf::from("/private/tmp/storyhook-lifecycle/store.db"),
            tailnet: None,
            cookie_name: "storyhook_test".to_string(),
        };
        assert!(!info.is_this_binary());
    }

    /// The same version rebuilt is the common developer case, and the one that
    /// produced the stale-daemon bug: the version matches, and the code does
    /// not.
    #[test]
    fn a_rebuild_of_the_same_version_is_not_this_binary() {
        let (exe, mtime) = current_binary().expect("this binary");
        let info = DaemonInfo {
            pid: 1,
            port: 1,
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: PROTOCOL,
            exe,
            exe_mtime: mtime + 1,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            token: "t".to_string(),
            store_path: PathBuf::from("/private/tmp/storyhook-lifecycle/store.db"),
            tailnet: None,
            cookie_name: "storyhook_test".to_string(),
        };
        assert!(
            !info.is_this_binary(),
            "a daemon serving the previous build of this version must not be reused"
        );
    }

    #[test]
    fn this_binary_matches_itself() {
        let info = info_for(
            &loopback_only(1),
            "t".to_string(),
            "2026-01-01T00:00:00Z",
            Path::new("/private/tmp/storyhook-lifecycle/store.db"),
        )
        .expect("building the info");
        assert!(info.is_this_binary());
    }

    #[test]
    fn a_token_is_unpredictable_and_url_safe() {
        let first = mint_token();
        let second = mint_token();
        assert_ne!(first, second);
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The bound is the sum of the waits it covers, and this is the assertion
    /// that keeps it so.
    ///
    /// A step added inside the critical section without a matching term here
    /// makes the bound smaller than the work it guards, and a client would then
    /// abandon a spawn that was going to succeed. Counted by naming each wait
    /// rather than by restating the number, so the failure says which one moved.
    #[test]
    fn the_spawn_lock_bound_is_the_sum_of_what_it_waits_on() {
        // request_shutdown against a stale daemon, then hello and another
        // request_shutdown inside stand_down_legacy_daemon.
        let control_waits = 3;
        // wait_until after each of those two shutdowns, then await_healthy.
        let spawn_waits = 3;
        assert_eq!(
            SPAWN_LOCK_DEADLINE,
            control_waits * CONTROL_DEADLINE + spawn_waits * SPAWN_DEADLINE,
            "the spawn-lock bound must cover every wait its holder can make"
        );
        assert_eq!(SPAWN_LOCK_DEADLINE, Duration::from_secs(30));
    }

    /// A record naming `command`, with `served_deadline_secs` set to exactly
    /// what production [`served_deadline`] would compute for it at a `cwd`
    /// with no hooks configured — so a test's fixture cannot silently drift
    /// from what the daemon would actually publish.
    fn serving(command: &str, request_id: &str) -> CurrentRequest {
        CurrentRequest {
            request_id: request_id.to_string(),
            command: command.to_string(),
            project: None,
            pid: 4711,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            served_deadline_secs: served_deadline(command, Path::new("/")).as_secs(),
            cwd: PathBuf::from("/"),
        }
    }

    /// The whole family, ordered, and the one rule that orders it.
    ///
    /// The ordering is load-bearing rather than tidy: a client must not abandon
    /// the exchange sooner than it would have abandoned the waits that precede
    /// it, or it gives the daemon less time to do the work than to come up.
    #[test]
    fn the_daemon_deadline_family_is_ordered() {
        assert!(CONTROL_DEADLINE < SPAWN_LOCK_DEADLINE);
        assert!(SPAWN_LOCK_DEADLINE < SERVED_DEADLINE);
        assert!(SERVED_PATIENCE < SERVED_DEADLINE);
        assert!(RECORD_POLL < SERVED_PATIENCE);
        assert!(SERVED_DEADLINE < PR_CHECK_SERVED_DEADLINE);

        // GIT_HOOK_DEADLINE (SH-343): above SPAWN_DEADLINE, or a cold start
        // `await_healthy` itself would accept gets abandoned by the hook that
        // triggered it; below SPAWN_LOCK_DEADLINE, so contention is abandoned
        // rather than queued — correct here because the client ahead leaves a
        // warm daemon for the next hook, and every managed hook's own answer
        // (bar prepare-commit-msg's, which sanctions degrading) is discarded.
        assert!(SPAWN_DEADLINE < GIT_HOOK_DEADLINE);
        assert!(GIT_HOOK_DEADLINE < SPAWN_LOCK_DEADLINE);

        assert_eq!(SERVED_PATIENCE, 2 * SPAWN_DEADLINE);
        // 120 consecutive worst-case GitHub calls, each bounded at 30s by
        // `GithubClient`. Stated as the arithmetic rather than as 3600 so that
        // moving the client's own bound is visible here.
        assert_eq!(PR_CHECK_SERVED_DEADLINE, 120 * Duration::from_secs(30));
        assert_eq!(SERVED_DEADLINE, Duration::from_secs(120));
        assert_eq!(GIT_HOOK_DEADLINE, Duration::from_secs(10));
    }

    /// Only `pr-check` gets the long bound, with no hooks configured — the
    /// one command whose cost is a function of GitHub API calls rather than
    /// of any one project's hooks.
    ///
    /// There is no longer a matching "nothing is ever unbounded" half:
    /// [`served_deadline`] returns a bare [`Duration`], so an unbounded
    /// result is not a value the type can hold, rather than a case a runtime
    /// check has to rule out.
    #[test]
    fn only_pr_check_gets_the_long_bound_with_no_hooks_configured() {
        assert_eq!(
            served_deadline("pr-check", Path::new("/")),
            PR_CHECK_SERVED_DEADLINE
        );
        for command in [
            "list",
            "new",
            "comment",
            "import",
            "export",
            "commit-sync",
            "doctor",
            "bulk-update",
            "purge",
            "graph",
            // Retired (SH-408): the verb no longer exists, and this proves it
            // is no longer special-cased either — an unrecognized command
            // string gets the ordinary bound, not a stale long one.
            "github-sync",
            "",
        ] {
            assert_eq!(
                served_deadline(command, Path::new("/")),
                SERVED_DEADLINE,
                "`{command}` must get the ordinary bound when no hooks are configured"
            );
        }
    }

    /// The ordinary bound widens by the largest hook timeout the project at
    /// `cwd` configures — the mitigation for the regression a dispatch pool
    /// introduces: "the set kept moving" no longer says anything about *my*
    /// entry once more than one can be in flight, so my own served time
    /// needs a bound that accounts for my own project's slowest hook.
    #[test]
    fn the_ordinary_bound_widens_by_the_largest_configured_hook_timeout() {
        let dir = scratch();
        std::fs::create_dir_all(dir.path().join(".storyhook")).expect("the hooks directory");
        std::fs::write(
            dir.path().join(".storyhook/hooks.toml"),
            "[settings]\ntimeout_seconds = 10\n\n\
             [on_comment]\ncommand = \"true\"\n\n\
             [on_create]\ncommand = \"true\"\ntimeout_seconds = 45\n",
        )
        .expect("writing hooks.toml");

        assert_eq!(
            served_deadline("comment", dir.path()),
            SERVED_DEADLINE + Duration::from_secs(45),
            "the largest configured hook wins, not the one `comment` would actually fire"
        );
        // Untouched: pr-check's total is set off this machine, not by any
        // project's hook configuration.
        assert_eq!(
            served_deadline("pr-check", dir.path()),
            PR_CHECK_SERVED_DEADLINE
        );
    }

    /// `set-state` and `set-fields` widen by the transition pair's *sum*, not
    /// the largest-of-all-seven every other command uses (SH-174).
    /// `fire_transition_hooks` fires `on_state_change` and `on_close`,
    /// serially, in one call when a transition closes a story — a widening
    /// based on the larger of the two alone would be exceeded by their sum,
    /// which is exactly the under-count this test pins closed.
    #[test]
    fn set_state_and_set_fields_widen_by_the_transition_pair_sum() {
        let dir = scratch();
        std::fs::create_dir_all(dir.path().join(".storyhook")).expect("the hooks directory");
        std::fs::write(
            dir.path().join(".storyhook/hooks.toml"),
            "[settings]\ntimeout_seconds = 10\n\n\
             [on_state_change]\ncommand = \"true\"\ntimeout_seconds = 40\n\n\
             [on_close]\ncommand = \"true\"\ntimeout_seconds = 15\n\n\
             [on_create]\ncommand = \"true\"\ntimeout_seconds = 60\n",
        )
        .expect("writing hooks.toml");

        for command in ["set-state", "set-fields"] {
            assert_eq!(
                served_deadline(command, dir.path()),
                SERVED_DEADLINE + Duration::from_secs(55),
                "`{command}` must widen by on_state_change (40) + on_close (15) = 55, \
                 not max(40, 15, 60) from on_create, which it can never fire"
            );
        }
        // Every other command still gets the max-of-all-seven widening,
        // unchanged: on_create's 60s wins there, because those commands
        // really can only ever fire one hook per call.
        assert_eq!(
            served_deadline("comment", dir.path()),
            SERVED_DEADLINE + Duration::from_secs(60)
        );
    }

    /// [`served_deadline_for`] widens `bulk-update` by `updates.len()` times
    /// the transition pair, not one widening's worth of it regardless of how
    /// many items are given (SH-174) — the "affected stories" gap this
    /// story's own description names.
    #[test]
    fn bulk_update_widens_by_n_times_the_transition_pair() {
        let dir = scratch();
        std::fs::create_dir_all(dir.path().join(".storyhook")).expect("the hooks directory");
        std::fs::write(
            dir.path().join(".storyhook/hooks.toml"),
            "[settings]\ntimeout_seconds = 10\n\n\
             [on_state_change]\ncommand = \"true\"\ntimeout_seconds = 40\n\n\
             [on_close]\ncommand = \"true\"\ntimeout_seconds = 15\n",
        )
        .expect("writing hooks.toml");

        let three_updates = crate::cli::Invocation::BulkUpdate {
            updates: vec![
                ("SH-1".to_string(), "done".to_string()),
                ("SH-2".to_string(), "done".to_string()),
                ("SH-3".to_string(), "done".to_string()),
            ],
        };
        assert_eq!(
            served_deadline_for(&three_updates, dir.path()),
            SERVED_DEADLINE + Duration::from_secs(55 * 3),
            "3 items x (40 + 15) per item, not one widening's worth for the whole call"
        );

        let no_updates = crate::cli::Invocation::BulkUpdate { updates: vec![] };
        assert_eq!(
            served_deadline_for(&no_updates, dir.path()),
            SERVED_DEADLINE + Duration::from_secs(55),
            "an empty batch still costs at least one widening's worth, not zero"
        );
    }

    /// Every non-`BulkUpdate` invocation delegates to [`served_deadline`]
    /// unchanged — [`served_deadline_for`] only special-cases the one command
    /// whose cost is not a function of a single hook chain.
    #[test]
    fn served_deadline_for_delegates_for_every_non_bulk_invocation() {
        let dir = scratch();
        std::fs::create_dir_all(dir.path().join(".storyhook")).expect("the hooks directory");
        std::fs::write(
            dir.path().join(".storyhook/hooks.toml"),
            "[settings]\ntimeout_seconds = 10\n\n\
             [on_state_change]\ncommand = \"true\"\ntimeout_seconds = 40\n\n\
             [on_close]\ncommand = \"true\"\ntimeout_seconds = 15\n",
        )
        .expect("writing hooks.toml");

        assert_eq!(
            served_deadline_for(&crate::cli::Invocation::Export, dir.path()),
            served_deadline("export", dir.path())
        );
        assert_eq!(
            served_deadline_for(
                &crate::cli::Invocation::SetState {
                    id: "SH-1".to_string(),
                    state: "done".to_string(),
                    comment: None,
                    if_state: None,
                    awaiting: None,
                },
                dir.path()
            ),
            served_deadline("set-state", dir.path()),
            "a lone set-state still gets the sum-of-the-pair widening via served_deadline, \
             just not multiplied by an item count"
        );
    }

    /// **The regression that matters most**, and the one every wall-clock design
    /// fails: a client queued behind work that is *moving* waits arbitrarily
    /// long, because the daemon is finishing things.
    ///
    /// Expressed against the pure decision rather than against a clock, so the
    /// assertion is "at any queue length" rather than "at the one length a test
    /// had patience for".
    #[test]
    fn a_client_queued_behind_moving_work_waits_however_long_it_takes() {
        let theirs = serving("pr-check", "not-mine");
        let inflight = [theirs];
        for hours in [1_u64, 6, 24, 24 * 365] {
            let seen = Observed {
                mine: "mine",
                inflight: &inflight,
                mine_for: None,
                // The set changed a moment ago, which is what "moving" means.
                since_change: Duration::from_millis(1),
                total: Duration::from_secs(hours * 3600),
            };
            assert_eq!(
                verdict(&seen, None),
                Verdict::Wait,
                "a moving set must never be given up on, even after {hours}h"
            );
        }
    }

    /// My own entry, frozen past its own deadline, fires — even with nothing
    /// else in the set.
    #[test]
    fn my_own_frozen_entry_is_given_up_on() {
        let mine = serving("import", "mine");
        let inflight = [mine.clone()];
        let seen = Observed {
            mine: "mine",
            inflight: &inflight,
            mine_for: Some(SERVED_DEADLINE),
            since_change: SERVED_DEADLINE,
            total: SERVED_DEADLINE,
        };
        assert_eq!(verdict(&seen, None), Verdict::GiveUpMine(mine));
    }

    /// A frozen entry fires even when it names a stranger — the daemon has
    /// still stopped finishing things, and a queued client is still blocked
    /// behind it. The case a naive design gets wrong.
    #[test]
    fn a_frozen_entry_is_given_up_on_even_when_it_names_a_stranger() {
        let theirs = serving("comment", "somebody-else");
        let inflight = [theirs.clone()];
        let seen = Observed {
            mine: "mine",
            inflight: &inflight,
            mine_for: None,
            since_change: SERVED_DEADLINE,
            total: SERVED_DEADLINE,
        };
        assert_eq!(verdict(&seen, None), Verdict::GiveUpQueued(vec![theirs]));
    }

    /// The deadline a queued client is bound by is a property of the record
    /// it is behind, never of the command it sent itself.
    ///
    /// The correction the council made to its own first answer, still true
    /// under a pool: keying on my own invocation would give the shortest
    /// patience to `story list`, the command whose wait is most inflated by
    /// other people's work.
    #[test]
    fn the_deadline_belongs_to_the_record_i_am_queued_behind() {
        let sync = serving("pr-check", "not-mine");
        let inflight = [sync.clone()];
        // Well past the ordinary bound, nowhere near the sync one.
        let still_waiting = Observed {
            mine: "mine",
            inflight: &inflight,
            mine_for: None,
            since_change: SERVED_DEADLINE,
            total: SERVED_DEADLINE,
        };
        assert_eq!(
            verdict(&still_waiting, None),
            Verdict::Wait,
            "a frozen pr-check must get the long bound even for a client that sent `list`"
        );
        let past_it = Observed {
            mine: "mine",
            inflight: &inflight,
            mine_for: None,
            since_change: PR_CHECK_SERVED_DEADLINE,
            total: PR_CHECK_SERVED_DEADLINE,
        };
        assert_eq!(verdict(&past_it, None), Verdict::GiveUpQueued(vec![sync]));
    }

    /// A queued client is bound by the **most patient** deadline in the set,
    /// never the least — one slow `pr-check` buys every neighbour its
    /// hour, because a queued client is behind all of it, not whichever
    /// entry happens to be fastest.
    #[test]
    fn one_slow_entry_in_the_set_buys_every_queued_client_its_patience() {
        let comment = serving("comment", "also-not-mine");
        let sync = serving("pr-check", "not-mine");
        let inflight = [comment, sync];
        let seen = Observed {
            mine: "mine",
            inflight: &inflight,
            mine_for: None,
            since_change: SERVED_DEADLINE,
            total: SERVED_DEADLINE,
        };
        assert_eq!(
            verdict(&seen, None),
            Verdict::Wait,
            "the set's slowest deadline must govern, not its fastest"
        );
    }

    /// A daemon that publishes nothing at all gets its own branch and its own
    /// deadline — the only shape the unauthenticated wedge can present.
    #[test]
    fn a_daemon_that_publishes_nothing_is_given_up_on_separately() {
        let empty: [CurrentRequest; 0] = [];
        let past_it = Observed {
            mine: "mine",
            inflight: &empty,
            mine_for: None,
            since_change: Duration::ZERO,
            total: UNPUBLISHED_DEADLINE,
        };
        assert_eq!(verdict(&past_it, None), Verdict::GaveUpUnpublished);

        let not_yet = Observed {
            mine: "mine",
            inflight: &empty,
            mine_for: None,
            since_change: Duration::ZERO,
            total: UNPUBLISHED_DEADLINE - Duration::from_secs(1),
        };
        assert_eq!(verdict(&not_yet, None), Verdict::Wait);
    }

    /// The hatch's `none`, which is the only thing in storyhook that can produce
    /// an unbounded wait — for my own entry and for the unpublished case alike.
    #[test]
    fn the_override_can_switch_the_bound_off_entirely() {
        let mine = serving("import", "mine");
        let inflight = [mine];
        let forever = Duration::from_secs(24 * 3600);
        let with_mine = Observed {
            mine: "mine",
            inflight: &inflight,
            mine_for: Some(forever),
            since_change: forever,
            total: forever,
        };
        assert_eq!(
            verdict(&with_mine, Some(ExchangeBound::Unbounded)),
            Verdict::Wait
        );

        let empty: [CurrentRequest; 0] = [];
        let with_nothing = Observed {
            mine: "mine",
            inflight: &empty,
            mine_for: None,
            since_change: forever,
            total: forever,
        };
        assert_eq!(
            verdict(&with_nothing, Some(ExchangeBound::Unbounded)),
            Verdict::Wait
        );
    }

    /// The override replaces every deadline, including the long one and the
    /// unpublished one — which is also what lets a test drive a 120s bound in
    /// 250ms.
    #[test]
    fn the_override_replaces_every_deadline() {
        let short = Some(ExchangeBound::After(Duration::from_millis(250)));
        let sync = serving("pr-check", "not-mine");
        let inflight = [sync.clone()];
        let queued = Observed {
            mine: "mine",
            inflight: &inflight,
            mine_for: None,
            since_change: Duration::from_millis(250),
            total: Duration::ZERO,
        };
        assert_eq!(
            verdict(&queued, short),
            Verdict::GiveUpQueued(vec![sync]),
            "the override must shorten even the pr-check bound"
        );

        let empty: [CurrentRequest; 0] = [];
        let unpublished = Observed {
            mine: "mine",
            inflight: &empty,
            mine_for: None,
            since_change: Duration::ZERO,
            total: Duration::from_millis(250),
        };
        assert_eq!(verdict(&unpublished, short), Verdict::GaveUpUnpublished);
    }

    /// The override shortens my own entry's deadline too, not only the
    /// queued case — the whole point being that a user who says "wait 10
    /// minutes" means it whatever the daemon is or is not saying.
    #[test]
    fn the_override_shortens_my_own_deadline_too() {
        let short = Some(ExchangeBound::After(Duration::from_millis(250)));
        let mine = serving("pr-check", "mine");
        let inflight = [mine.clone()];
        let seen = Observed {
            mine: "mine",
            inflight: &inflight,
            mine_for: Some(Duration::from_millis(250)),
            since_change: Duration::from_millis(250),
            total: Duration::from_millis(250),
        };
        assert_eq!(verdict(&seen, short), Verdict::GiveUpMine(mine));
    }

    /// The defect itself, at the smallest level it exists: a held lock used to
    /// block a waiter forever.
    #[test]
    fn a_held_spawn_lock_is_given_up_on_rather_than_waited_out() {
        let dir = scratch();
        let path = dir.path().join("daemon.spawn.lock");
        let _held = acquire_spawn_lock(&path, Duration::from_secs(5)).expect("a free lock");

        let started = Instant::now();
        let second = acquire_spawn_lock(&path, Duration::from_millis(250));
        let waited = started.elapsed();

        let error = second.expect_err("a lock somebody else holds must not be waited out");
        assert!(
            error.to_string().contains("spawn lock"),
            "the failure must name what it could not take: {error}"
        );
        assert!(
            waited < Duration::from_secs(2),
            "the wait must end at its deadline, not somewhere after it: took {waited:?}"
        );
    }

    /// The other half: the bound must not cost anything when nobody is holding
    /// the lock, which is every ordinary command.
    #[test]
    fn a_free_spawn_lock_is_taken_immediately() {
        let dir = scratch();
        let path = dir.path().join("daemon.spawn.lock");

        let started = Instant::now();
        let lock = acquire_spawn_lock(&path, SPAWN_LOCK_DEADLINE).expect("a free lock");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "an uncontended acquire must not pay a poll interval"
        );

        // And it is released for the next one, rather than held to the end of
        // the process by the file staying open.
        let _ = FileExt::unlock(&lock);
        drop(lock);
        acquire_spawn_lock(&path, Duration::from_millis(250)).expect("the lock was released");
    }

    /// A waiter adopts a verdict that arrived *while it waited*, and refuses one
    /// that predates it. The freshness comparison is the whole safety of
    /// believing another process's answer.
    #[test]
    fn a_verdict_is_adopted_only_when_it_postdates_this_client_arriving() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the daemon directory");

        let before = std::time::SystemTime::now();
        // A moment's separation, because a filesystem timestamp is not infinitely
        // precise and this test is about an ordering, not about a tie.
        std::thread::sleep(Duration::from_millis(50));
        publish_attempt(
            &env,
            &Err(AppError::Storage("the daemon would not start".to_string())),
        );
        std::thread::sleep(Duration::from_millis(50));
        let after = std::time::SystemTime::now();

        let adopted = attempt_verdict(&env, before).expect("a verdict written after we arrived");
        assert!(
            adopted.to_string().contains("the daemon would not start"),
            "the adopted answer must be the one the other client reached: {adopted}"
        );
        assert!(
            adopted.to_string().contains("another storyhook client"),
            "and it must say whose answer it is: {adopted}"
        );
        assert!(
            attempt_verdict(&env, after).is_none(),
            "a verdict older than this client describes a world it has not seen"
        );
    }

    /// Success removes the record. A spent failure left behind is a claim about
    /// a world that has since been fixed.
    #[test]
    fn a_successful_attempt_retracts_the_failure_before_it() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the daemon directory");
        let arrived = std::time::SystemTime::now();
        std::thread::sleep(Duration::from_millis(50));

        publish_attempt(
            &env,
            &Err(AppError::Storage("it did not start".to_string())),
        );
        assert!(attempt_verdict(&env, arrived).is_some());

        let info = info_for(
            &loopback_only(4321),
            "t".to_string(),
            "2026-01-01T00:00:00Z",
            env.store_path(),
        )
        .expect("building the info");
        publish_attempt(&env, &Ok(info));

        assert!(
            attempt_verdict(&env, arrived).is_none(),
            "a daemon that started must not leave the last failure standing"
        );
        assert!(!env.daemon_attempt().exists());
    }

    /// The record is published beside the lock and never over it: an `flock`
    /// lives on the inode, so a rename onto the lock file would hand the next
    /// waiter a different inode from the one the holder holds, and the mutual
    /// exclusion would quietly stop existing.
    #[test]
    fn the_attempt_record_is_never_the_lock_file() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert_ne!(env.daemon_attempt(), env.daemon_spawn_lock());
        assert_eq!(
            env.daemon_attempt().parent(),
            env.daemon_spawn_lock().parent()
        );
    }

    #[test]
    fn stopping_nothing_is_not_an_error() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert_eq!(stop(&env, StopMode::Force).expect("stopping nothing"), None);
    }

    /// A daemon that crashed leaves its portfile behind. `stop` clears it, so
    /// `status` stops describing a process that is gone.
    #[test]
    fn stopping_clears_a_portfile_left_by_a_crashed_daemon() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let info = info_for(
            &loopback_only(4321),
            "t".to_string(),
            "2026-01-01T00:00:00Z",
            env.store_path(),
        )
        .expect("building the info");
        write_info(&env, &info).expect("writing the portfile");
        assert_eq!(stop(&env, StopMode::Force).expect("stopping"), None);
        assert!(!env.daemon_file().exists());
    }

    /// The evidence a crash left is harvested before `stop` clears the
    /// portfile that names it, so a human running `story daemon stop` after a
    /// crash still gets a ledger entry rather than erasing the only sign
    /// anything happened (SH-287).
    #[test]
    fn stopping_a_crashed_daemon_ledgers_it_before_clearing_the_portfile() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let info = info_for(
            &loopback_only(4321),
            "t".to_string(),
            "2026-01-01T00:00:00Z",
            env.store_path(),
        )
        .expect("building the info");
        write_info(&env, &info).expect("writing the portfile");

        assert_eq!(stop(&env, StopMode::Force).expect("stopping"), None);

        let ledgered = crate::daemon::crash::read_crashes(&env);
        assert_eq!(ledgered.len(), 1, "stop must harvest the crash it clears");
        assert_eq!(
            ledgered[0].classification,
            crate::daemon::crash::CrashClassification::UncleanExit
        );
    }

    /// A record naming `command`, for the ledger tests below — otherwise
    /// uninteresting, so its own deadline does not matter.
    fn abandoned_request(command: &str, request_id: &str) -> CurrentRequest {
        CurrentRequest {
            request_id: request_id.to_string(),
            command: command.to_string(),
            project: Some("PB".to_string()),
            pid: 4711,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            served_deadline_secs: SERVED_DEADLINE.as_secs(),
            cwd: PathBuf::from("/"),
        }
    }

    #[test]
    fn an_empty_slice_never_touches_the_ledger_file() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the state dir");
        ledger_abandoned(&env, &[], "should never be reached");
        assert!(!env.daemon_abandoned().exists());
    }

    #[test]
    fn abandoned_entries_accumulate_across_separate_calls() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the state dir");

        ledger_abandoned(
            &env,
            &[abandoned_request("comment", "first")],
            "the first reason",
        );
        ledger_abandoned(
            &env,
            &[abandoned_request("pr-check", "second")],
            "the second reason",
        );

        let ledger = read_abandoned(&env);
        assert_eq!(
            ledger.len(),
            2,
            "both calls must be preserved, not overwritten"
        );
        assert_eq!(ledger[0].request.request_id, "first");
        assert_eq!(ledger[0].reason, "the first reason");
        assert_eq!(ledger[1].request.request_id, "second");
        assert_eq!(ledger[1].reason, "the second reason");
    }

    #[test]
    fn clearing_one_entry_leaves_the_others() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the state dir");
        ledger_abandoned(
            &env,
            &[
                abandoned_request("comment", "keep-me"),
                abandoned_request("comment", "forget-me"),
            ],
            "a reason",
        );

        assert!(clear_abandoned(&env, Some("forget-me")));
        let ledger = read_abandoned(&env);
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].request.request_id, "keep-me");

        assert!(
            !clear_abandoned(&env, Some("not-in-the-ledger")),
            "clearing an id that was never there must report nothing changed"
        );
    }

    #[test]
    fn clearing_everything_empties_the_file() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the state dir");
        ledger_abandoned(
            &env,
            &[
                abandoned_request("comment", "a"),
                abandoned_request("comment", "b"),
            ],
            "a reason",
        );

        assert!(clear_abandoned(&env, None));
        assert!(read_abandoned(&env).is_empty());
        assert!(!env.daemon_abandoned().exists());
    }

    /// `harvest_stale` does not merely clear a stale record — it ledgers it,
    /// because a record that survived to here means the daemon that named it
    /// did not exit normally, and that is exactly what the ledger is for.
    #[test]
    fn harvest_stale_ledgers_what_it_finds() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the state dir");
        publish_inflight(&env, &[abandoned_request("pr-check", "stale-one")]);

        let inflight = InFlight::new(env.clone());
        inflight.harvest_stale();

        assert!(
            read_inflight(&env).is_empty(),
            "the stale record must still be cleared"
        );
        let ledger = read_abandoned(&env);
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].request.request_id, "stale-one");
    }
}
