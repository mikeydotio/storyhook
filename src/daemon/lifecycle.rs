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
    bound: SocketAddr,
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
) -> Result<(Vec<super::serve::Listener>, SocketAddr, Vec<String>), AppError> {
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
    let (listeners, bound, mut trusted_hosts) = bind_preferred(env)?;
    trusted_hosts.extend(crate::api::http::trusted_hosts_from_env());

    let info = info_for(bound, mint_token(), &env.now(), env.store_path())?;
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

/// The slow path: hold the spawn lock, re-check, replace what is there, start a
/// daemon, and keep holding until it answers.
fn spawn_locked(env: &Environment) -> Result<DaemonInfo, AppError> {
    std::fs::create_dir_all(env.daemon_state_dir())?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(env.daemon_spawn_lock())
        .map_err(|e| AppError::Storage(format!("failed to open the daemon spawn lock: {e}")))?;
    lock.lock_exclusive()
        .map_err(|e| AppError::Storage(format!("failed to take the daemon spawn lock: {e}")))?;

    // Re-check under the lock: another client may have started one while this
    // one was waiting for the lock, and starting a second would be the whole
    // point of the lock being missed.
    let outcome = (|| -> Result<DaemonInfo, AppError> {
        if let Some(info) = usable(env) {
            return Ok(info);
        }

        // Something is there that is not ours. Ask it to stand down before
        // taking its place: it holds the pidfile lock, and a new daemon cannot
        // start while it does.
        if let Some(stale) = read_info(env)
            && is_live(env)
        {
            let _ = request_shutdown(&stale);
            wait_until(SPAWN_DEADLINE, || !is_live(env));
        }

        stand_down_legacy_daemon(env);
        spawn_child(env)?;
        await_healthy(env)
    })();

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

/// Starts a detached daemon process.
///
/// It inherits this process's environment, which is how a test harness's
/// isolated data and state homes reach it. `STORYHOOK_PARENT_PID` travels with
/// it too: a daemon started by a test binary must not outlive that binary, and
/// the daemon polls for its parent rather than trusting anybody to reap it.
fn spawn_child(env: &Environment) -> Result<(), AppError> {
    let (exe, _) = current_binary()?;
    std::fs::create_dir_all(env.daemon_state_dir())?;
    let log = File::create(env.daemon_log())
        .map_err(|e| AppError::Storage(format!("failed to create the daemon log: {e}")))?;

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
    command
        .spawn()
        .map_err(|e| AppError::Storage(format!("failed to start the storyhook daemon: {e}")))?;
    Ok(())
}

/// Polls until a daemon of this build is answering, or gives up loudly.
fn await_healthy(env: &Environment) -> Result<DaemonInfo, AppError> {
    let deadline = Instant::now() + SPAWN_DEADLINE;
    while Instant::now() < deadline {
        if let Some(info) = read_info(env)
            && info.is_this_binary()
            && info.serves(env.store_path())
            && hello(&info).is_ok()
        {
            return Ok(info);
        }
        std::thread::sleep(SPAWN_POLL);
    }
    Err(AppError::Storage(format!(
        "the storyhook daemon did not start within {}s. Its log is at {}; \
         `story --local <command>` runs without it.",
        SPAWN_DEADLINE.as_secs(),
        env.daemon_log().display()
    )))
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
const CONTROL_DEADLINE: Duration = Duration::from_secs(5);

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

    #[test]
    fn a_portfile_round_trips_through_disk() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let info = info_for(
            SocketAddr::from(([127, 0, 0, 1], 4321)),
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
            SocketAddr::from(([127, 0, 0, 1], 4321)),
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
            SocketAddr::from(([127, 0, 0, 1], 4321)),
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
        };
        assert!(
            !info.is_this_binary(),
            "a daemon serving the previous build of this version must not be reused"
        );
    }

    #[test]
    fn this_binary_matches_itself() {
        let info = info_for(
            SocketAddr::from(([127, 0, 0, 1], 1)),
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
            SocketAddr::from(([127, 0, 0, 1], 4321)),
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
