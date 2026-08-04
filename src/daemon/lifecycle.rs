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
const SPAWN_DEADLINE: Duration = Duration::from_secs(5);

/// How often it asks, while waiting.
const SPAWN_POLL: Duration = Duration::from_millis(25);

/// How long a shutting-down daemon lets in-flight requests finish.
pub const DRAIN_DEADLINE: Duration = Duration::from_millis(500);

/// What the daemon is serving right now.
///
/// **The record whose motion is the client's clock.** Published when a request's
/// envelope has parsed and removed when its answer is ready, so it changes
/// exactly when the daemon finishes something — and a client waiting on a
/// command it cannot see any bytes from can therefore tell "the daemon is
/// working through a queue" from "the daemon has stopped finishing things"
/// without asking it anything (SH-144).
///
/// Every field answers one question the failure message has to answer.
/// `request_id` says *is this mine*; `command` chooses the deadline and names
/// the work; `project` says whose; `pid` is the way out, because
/// `story daemon stop` has no signal fallback; `started_at` says how long.
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
}

/// Publishes what the daemon is serving, atomically.
///
/// Temp-plus-`rename` and mode 0600, the same shape as [`write_info`] and for
/// the same reason: a client that read a half-written record would see a
/// `request_id` that matches nothing and conclude it was queued when it is
/// being served.
///
/// **Best-effort, and never fatal.** A daemon whose state directory is full or
/// read-only must still run the command it was asked to run — failing here
/// would replace a good answer with a bad diagnostic about a diagnostic, which
/// is the rule [`record_startup_failure`] already states for the same reason.
pub fn publish_current(env: &Environment, current: &CurrentRequest) {
    let Ok(document) = serde_json::to_string(current) else {
        return;
    };
    let path = env.daemon_current();
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

/// Retracts the current-request record, because the request is answered.
///
/// Removal is a signal in its own right: the client's clock resets on it, the
/// same as on a change. Best-effort for the same reason as [`publish_current`].
pub fn clear_current(env: &Environment) {
    let _ = std::fs::remove_file(env.daemon_current());
}

/// Reads what a daemon says it is serving, or `None` if it says nothing.
///
/// `None` covers three states a client must not conflate with each other, and
/// the caller distinguishes them: the daemon is between requests, the daemon
/// never published (a state the record cannot describe — see SH-144's "the
/// record never appears" branch), or the file is being replaced right now. The
/// last is why a parse failure is `None` rather than an error: the write is
/// atomic, so a torn read is not possible, but a *stale* document from an older
/// binary is — and `usable` has already refused that daemon.
pub fn read_current(env: &Environment) -> Option<CurrentRequest> {
    let raw = std::fs::read_to_string(env.daemon_current()).ok()?;
    serde_json::from_str(&raw).ok()
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
fn write_info(env: &Environment, info: &DaemonInfo) -> Result<(), AppError> {
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
fn mint_token() -> String {
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
    })
}

/// The port a daemon should try first.
///
/// The preferred port keeps a bookmarked dashboard URL working across restarts;
/// falling back to an OS-assigned one means a daemon can always start, even when
/// something else holds it. A test harness sets the preferred port to 0, so a
/// suite never contends for the port a developer's own dashboard is using.
pub fn preferred_port(env: &Environment) -> u16 {
    env.preferred_daemon_addr().port()
}

/// Binds a listener for the daemon: the preferred port, else one the kernel
/// picks.
///
/// Falling back rather than failing is what makes "port already in use" stop
/// being a startup failure at all. The portfile is authoritative about where the
/// daemon actually is, so nothing downstream needs the preferred port to have
/// been available.
pub fn bind_preferred(
    env: &Environment,
) -> Result<(Vec<super::serve::Listener>, super::serve::BoundAddress), AppError> {
    let preferred = preferred_port(env);
    match super::serve::bind_listeners(preferred) {
        Ok(bound) => Ok(bound),
        Err(_) if preferred != 0 => super::serve::bind_listeners(0),
        Err(e) => Err(e),
    }
}

/// Runs the daemon in this process, until it is asked to stop.
///
/// The order is the contract: take the lifetime lock (so a second daemon cannot
/// start), bind (so the port is real), publish the portfile (so a client can
/// find it), then serve.
pub fn run<S: crate::store::Store>(store: &S, env: &Environment) -> Result<(), AppError> {
    let _pidfile = claim_pidfile(env)?;
    let (listeners, bound) = bind_preferred(env)?;
    let mut trusted_hosts = bound.trusted_hosts();
    trusted_hosts.extend(crate::api::http::trusted_hosts_from_env());

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
    super::serve::serve(
        store,
        env,
        listeners,
        trusted_hosts,
        bus,
        info.token.clone(),
        || {},
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
    let log = File::create(env.daemon_log())
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
    let port = preferred_port(env).to_string();
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
/// as the longest `github-sync` it might be queued behind, or it would abandon
/// healthy work (SH-144).
///
/// # Where the number comes from
///
/// **Calibrated, not derived, and that distinction is deliberate.** Its
/// neighbour [`SPAWN_LOCK_DEADLINE`] can say "derived, never chosen" because
/// every wait it covers is storyhook's own bounded code. A handler runs the
/// *user's* data through the user's event hooks, and
/// `hooks.settings.timeout_seconds` has no ceiling, so no honest derivation
/// exists yet. A fake one would be worse than a calibration, because it would
/// survive a review it should not.
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
pub const SERVED_DEADLINE: Duration = Duration::from_secs(120);

/// The same bound, for the one command whose duration is set off this machine.
///
/// `github-sync` loops per story and per comment against a client that bounds
/// each call at 30s, so its total is O(stories x comments) x 30s and cannot be
/// calibrated from local measurement at all. 3600s is **120 consecutive
/// worst-case calls**: a sync in which that many in a row exhaust their timeout
/// is throttled or offline, and stopping is the right answer.
///
/// **A longer value, never no value.** An exemption was proposed and rejected:
/// it would restore the forever-hang this work exists to remove — including a
/// `github-sync` wedged in a terminal-less `Select::interact()` (SH-153), which
/// would then never fire at all. One unbounded arm anywhere means the story has
/// not shipped.
pub const SYNC_SERVED_DEADLINE: Duration = Duration::from_secs(3600);

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
/// The branch that exists because the record can be absent for a reason no
/// record can describe: the daemon accepted the connection and never reached
/// [`publish_current`]. Post-SH-144 this is the only shape the unauthenticated
/// wedge can present to a client, and the reason it gets its own constant rather
/// than sharing [`SERVED_DEADLINE`] is that its message is different — a daemon
/// that is alive, holding its pidfile, and has not started your command.
pub const UNPUBLISHED_DEADLINE: Duration = Duration::from_secs(120);

/// How long a client will wait for the daemon to finish one command.
///
/// A type rather than a bare `Duration` so that "no bound" is a value somebody
/// chose rather than a `None` that reads as "unset". Nothing in storyhook's own
/// code produces [`Self::Unbounded`] — every command is bounded by
/// [`SERVED_DEADLINE`] or [`SYNC_SERVED_DEADLINE`] — so its **sole producer** is
/// a user writing `none` in `$STORYHOOK_EXCHANGE_DEADLINE_SECS`, which is
/// exactly the state worth making visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeBound {
    /// Give up when the record has not moved for this long.
    After(Duration),
    /// Wait forever, because the user asked for the pre-SH-144 behaviour.
    Unbounded,
}

/// The bound for the command **named in the record** — never the command this
/// client sent.
///
/// That distinction is the whole correction the council made to its own first
/// answer. The daemon serves one request at a time, so the thing that decides
/// how long a wait is legitimate is *what the daemon is doing*, not what I asked
/// for; keying on my own invocation would give the shortest patience to
/// `story list`, the command whose wait is most inflated by other people's work.
///
/// Keyed on the string, because the string is what crosses the file. It reads
/// the vocabulary [`crate::invoke::invocation_name`] writes.
#[must_use]
pub fn deadline_for(command: &str) -> ExchangeBound {
    match command {
        "github-sync" => ExchangeBound::After(SYNC_SERVED_DEADLINE),
        _ => ExchangeBound::After(SERVED_DEADLINE),
    }
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
    /// The daemon is finishing things. Keep waiting, however long that is.
    Wait,
    /// The record has not moved for the deadline of the command it names.
    GiveUp(CurrentRequest),
    /// The daemon published nothing at all for [`UNPUBLISHED_DEADLINE`].
    GaveUpUnpublished,
}

/// Decides whether to keep waiting.
///
/// `since_change` is how long the record has looked exactly as it looks now;
/// `total` is the whole wait. The two are different clocks and only one of them
/// is a deadline — `total` is consulted **only** when there has never been a
/// record to time, which is the one state the record itself cannot describe.
///
/// Note what is missing: there is no clock on queued time, and that is the
/// design rather than an omission. While the record keeps changing the daemon is
/// finishing work, so a client behind a long queue waits as long as the queue
/// takes. That is M9's false positive deleted rather than tuned.
/// `override_bound` is exactly what its name says: `None` means *use the
/// deadline belonging to the command the record names*, which is the production
/// path and the whole point of publishing the command at all. `Some` is the
/// user's `$STORYHOOK_EXCHANGE_DEADLINE_SECS`, and it is also how a test drives
/// a 120-second bound in 250 milliseconds.
#[must_use]
pub fn verdict(
    current: Option<&CurrentRequest>,
    since_change: Duration,
    total: Duration,
    override_bound: Option<ExchangeBound>,
) -> Verdict {
    if let Some(ExchangeBound::Unbounded) = override_bound {
        return Verdict::Wait;
    }
    // The override, if there is one, replaces every deadline below — including
    // the unpublished one, because a user who says "wait 10 minutes" means it
    // whatever the daemon is or is not saying.
    let deadline_of = |command: &str| match override_bound {
        Some(ExchangeBound::After(d)) => d,
        _ => match deadline_for(command) {
            ExchangeBound::After(d) => d,
            // Unreachable: `deadline_for` never answers `Unbounded`, and the
            // type is what stops that becoming a silent forever-wait if it ever
            // did.
            ExchangeBound::Unbounded => SERVED_DEADLINE,
        },
    };

    match current {
        // A record is present: the deadline is the one for the command it
        // names, whether or not that command is mine. A record frozen on
        // somebody else's request still means the daemon has stopped finishing
        // things, and it still blocks me.
        Some(record) if since_change >= deadline_of(&record.command) => {
            Verdict::GiveUp(record.clone())
        }
        Some(_) => Verdict::Wait,
        // No record. Either the daemon is between requests — in which case one
        // is about to appear and `since_change` keeps resetting — or it never
        // published at all, which is the branch below.
        None => {
            let unpublished = match override_bound {
                Some(ExchangeBound::After(d)) => d,
                _ => UNPUBLISHED_DEADLINE,
            };
            if total >= unpublished {
                Verdict::GaveUpUnpublished
            } else {
                Verdict::Wait
            }
        }
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

/// Asks a daemon to shut down, and waits for it to let go of its pidfile.
pub fn request_shutdown(info: &DaemonInfo) -> Result<(), AppError> {
    let url = format!("http://127.0.0.1:{}/api/v1/shutdown", info.port);
    control_agent()
        .post(&url)
        .header("X-Storyhook-Token", &info.token)
        .send_empty()
        .map_err(|e| AppError::Storage(format!("the daemon refused to shut down: {e}")))?;
    Ok(())
}

/// Stops the running daemon, if there is one.
///
/// Reports what it stopped, or `None` when nothing was running — a distinction
/// the caller renders, because "already stopped" is a success for `daemon stop`
/// and a surprise for a restart.
pub fn stop(env: &Environment) -> Result<Option<DaemonInfo>, AppError> {
    if !is_live(env) {
        // Nothing holds the lock. Clear a portfile left by a daemon that
        // crashed, so `daemon status` stops describing a process that is gone.
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
    if !wait_until(SPAWN_DEADLINE, || !is_live(env)) {
        return Err(AppError::Storage(format!(
            "the daemon on port {} did not stop within {}s",
            info.port,
            SPAWN_DEADLINE.as_secs()
        )));
    }
    let _ = std::fs::remove_file(env.daemon_file());
    Ok(Some(info))
}

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

/// Whether `path` is a file this process could execute.
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
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

    /// A record naming `command`, otherwise uninteresting.
    fn serving(command: &str, request_id: &str) -> CurrentRequest {
        CurrentRequest {
            request_id: request_id.to_string(),
            command: command.to_string(),
            project: None,
            pid: 4711,
            started_at: "2026-01-01T00:00:00Z".to_string(),
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
        assert!(SERVED_DEADLINE < SYNC_SERVED_DEADLINE);

        assert_eq!(SERVED_PATIENCE, 2 * SPAWN_DEADLINE);
        // 120 consecutive worst-case GitHub calls, each bounded at 30s by
        // `GithubClient`. Stated as the arithmetic rather than as 3600 so that
        // moving the client's own bound is visible here.
        assert_eq!(SYNC_SERVED_DEADLINE, 120 * Duration::from_secs(30));
        assert_eq!(SERVED_DEADLINE, Duration::from_secs(120));
    }

    /// Only `github-sync` gets the long bound, and nothing is ever exempt.
    ///
    /// The second half is the point. An exemption was proposed and rejected
    /// because it restores the forever-hang this work exists to remove; this
    /// asserts no command can reach an unbounded verdict.
    #[test]
    fn only_github_sync_is_slow_and_nothing_is_unbounded() {
        assert_eq!(
            deadline_for("github-sync"),
            ExchangeBound::After(SYNC_SERVED_DEADLINE)
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
            "",
        ] {
            assert_eq!(
                deadline_for(command),
                ExchangeBound::After(SERVED_DEADLINE),
                "`{command}` must get the ordinary bound"
            );
        }
        for command in ["github-sync", "list", "anything-added-tomorrow"] {
            assert!(
                !matches!(deadline_for(command), ExchangeBound::Unbounded),
                "no command may be unbounded: `{command}`"
            );
        }
    }

    /// **The regression that matters most**, and the one every wall-clock design
    /// fails: a client queued behind work that is *moving* waits arbitrarily
    /// long, because the daemon is finishing things.
    ///
    /// Expressed against the pure decision rather than against a clock, so the
    /// assertion is "at any queue length" rather than "at the one length a test
    /// had patience for".
    #[test]
    fn a_client_behind_moving_work_waits_however_long_it_takes() {
        let theirs = serving("github-sync", "not-mine");
        for hours in [1_u64, 6, 24, 24 * 365] {
            let total = Duration::from_secs(hours * 3600);
            assert_eq!(
                // The record changed a moment ago, which is what "moving" means.
                verdict(Some(&theirs), Duration::from_millis(1), total, None),
                Verdict::Wait,
                "a moving record must never be given up on, even after {hours}h"
            );
        }
    }

    /// A record that has stopped moving fires — whoever it names.
    ///
    /// Both halves matter. Frozen on my own request is the plain case; frozen on
    /// somebody else's is the one a naive design gets wrong, because the daemon
    /// has still stopped finishing things and I am still blocked behind it.
    #[test]
    fn a_frozen_record_is_given_up_on_whoever_it_names() {
        let mine = serving("import", "mine");
        assert_eq!(
            verdict(Some(&mine), SERVED_DEADLINE, SERVED_DEADLINE, None),
            Verdict::GiveUp(mine.clone())
        );

        let theirs = serving("comment", "somebody-else");
        assert_eq!(
            verdict(Some(&theirs), SERVED_DEADLINE, SERVED_DEADLINE, None),
            Verdict::GiveUp(theirs)
        );
    }

    /// The deadline is a property of the command *in the record*, never of the
    /// command this client sent.
    ///
    /// The correction the council made to its own first answer: keying on my own
    /// invocation would give the shortest patience to `story list`, the command
    /// whose wait is most inflated by other people's work.
    #[test]
    fn the_deadline_belongs_to_the_command_in_the_record() {
        let sync = serving("github-sync", "not-mine");
        // Well past the ordinary bound, nowhere near the sync one.
        assert_eq!(
            verdict(Some(&sync), SERVED_DEADLINE, SERVED_DEADLINE, None),
            Verdict::Wait,
            "a frozen github-sync must get the long bound even for a client that sent `list`"
        );
        assert_eq!(
            verdict(
                Some(&sync),
                SYNC_SERVED_DEADLINE,
                SYNC_SERVED_DEADLINE,
                None
            ),
            Verdict::GiveUp(sync)
        );
    }

    /// A daemon that publishes nothing at all gets its own branch and its own
    /// deadline — the only shape the unauthenticated wedge can present.
    #[test]
    fn a_daemon_that_publishes_nothing_is_given_up_on_separately() {
        assert_eq!(
            verdict(None, Duration::ZERO, UNPUBLISHED_DEADLINE, None),
            Verdict::GaveUpUnpublished
        );
        assert_eq!(
            verdict(
                None,
                Duration::ZERO,
                UNPUBLISHED_DEADLINE - Duration::from_secs(1),
                None
            ),
            Verdict::Wait
        );
    }

    /// The hatch's `none`, which is the only thing in storyhook that can produce
    /// an unbounded wait.
    #[test]
    fn the_override_can_switch_the_bound_off_entirely() {
        let mine = serving("import", "mine");
        let forever = Duration::from_secs(24 * 3600);
        assert_eq!(
            verdict(
                Some(&mine),
                forever,
                forever,
                Some(ExchangeBound::Unbounded)
            ),
            Verdict::Wait
        );
        assert_eq!(
            verdict(None, forever, forever, Some(ExchangeBound::Unbounded)),
            Verdict::Wait
        );
    }

    /// The override replaces every deadline, including the long one and the
    /// unpublished one — which is also what lets a test drive a 120s bound in
    /// 250ms.
    #[test]
    fn the_override_replaces_every_deadline() {
        let short = Some(ExchangeBound::After(Duration::from_millis(250)));
        let sync = serving("github-sync", "not-mine");
        assert_eq!(
            verdict(
                Some(&sync),
                Duration::from_millis(250),
                Duration::ZERO,
                short
            ),
            Verdict::GiveUp(sync),
            "the override must shorten even the github-sync bound"
        );
        assert_eq!(
            verdict(None, Duration::ZERO, Duration::from_millis(250), short),
            Verdict::GaveUpUnpublished
        );
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
        assert_eq!(stop(&env).expect("stopping nothing"), None);
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
        assert_eq!(stop(&env).expect("stopping"), None);
        assert!(!env.daemon_file().exists());
    }
}
