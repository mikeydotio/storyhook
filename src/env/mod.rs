//! [`Environment`] — everything storyhook reads from outside itself, resolved
//! once.
//!
//! Before this existed, the answer to "where does the store live" was a call to
//! [`std::env::var`] at whatever point in the program first needed to know, and
//! there were about ten such points. Two consequences, both paid for:
//!
//! 1. **Nothing could be redirected in-process.** A test harness isolates a
//!    child process by setting variables; an in-process caller has no such
//!    lever, so a service that read a global path from the environment wrote
//!    into the developer's real home no matter what the test wanted. That
//!    happened twice.
//! 2. **The clock was unmockable**, so anything derived from "now" — staleness,
//!    backup age — could only be tested by waiting.
//!
//! One value, built in `main`, passed down. A test constructs one pointing at a
//! scratch directory and gets the same isolation an environment variable gives a
//! child process.

pub mod git_env;

/// The credentials this process takes out of its own environment so that
/// nothing it spawns can inherit one.
///
/// Sibling of [`git_env`] and the same shape of problem: a daemon holds its
/// spawner's environment for life. A credential makes it sharper, because the
/// daemon hands that environment on to a user's hook script, the dashboard's
/// dispatch child and `claude`.
pub mod secrets;

/// The allowlists for `story.sh`'s dispatch child and `claude`'s plugin
/// subcommands — the two spawns storyhook itself trusts. Sibling of
/// [`git_env`], applying the same "deny at the process, allow at the command"
/// split to the two children SH-193 named that [`secrets`] and `git_env`
/// (SH-153, SH-160) do not cover.
pub mod spawn_env;
mod store_location;

/// What a storyhook **test environment** is: the environment variables that
/// stop a run reaching the developer's own store, daemon and credentials,
/// stated once so that no consumer has to hand-copy them.
///
/// Sibling of [`secrets`] and [`spawn_env`] in kind — all three are about what
/// a storyhook process may see of the machine around it — and the outermost of
/// the three: those two decide what storyhook's *children* inherit, this one
/// decides what storyhook itself does.
pub mod test_environment;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::AppError;
use crate::service::Clock;

pub(crate) use store_location::KEY_HEX;
pub use store_location::{StoreLocation, StoreOrigin, StoreVars, canonical_ish};

/// The file names inside a store's [`Environment::daemon_state_dir`], stated
/// once so the accessors below and `story daemon gc` — which reads a
/// directory it has no `Environment` for — cannot spell them apart (SH-638).
pub(crate) mod runtime_file {
    /// [`super::Environment::daemon_file`].
    pub const PORTFILE: &str = "daemon.json";
    /// [`super::Environment::daemon_pidfile`].
    pub const PIDFILE: &str = "daemon.pid";
    /// [`super::Environment::daemon_spawn_lock`].
    pub const SPAWN_LOCK: &str = "daemon.spawn.lock";
    /// [`super::Environment::daemon_log`].
    pub const LOG: &str = "daemon.log";
    /// [`super::Environment::daemon_log_rotated`].
    pub const LOG_ROTATED: &str = "daemon.log.1";
}

/// The port the daemon prefers, and the one the dashboard bookmark names.
///
/// Not a hard requirement: [`crate::daemon`] falls back to an OS-assigned port
/// when this one is taken, so a second machine-local daemon (or an unrelated
/// service) can never stop storyhook from starting.
pub const DEFAULT_DAEMON_PORT: u16 = 3456;

/// How long a writer waits for another process's write lock before giving up
/// with [`AppError::LockTimeout`].
///
/// Five seconds is SQLite's `busy_timeout` for this store, and it is deliberately
/// generous: the contention it covers is a human or a hook racing another
/// invocation, where failing fast would be worse than waiting.
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything storyhook reads from outside itself.
///
/// Construct one with [`Environment::from_process`] in `main`, or with
/// [`Environment::at`] in a test. Every path below is *resolved*: reading an
/// `Environment` never touches the process environment again, so passing one
/// into a service is what makes that service redirectable by an in-process
/// caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    store: StoreLocation,
    state_home: PathBuf,
    home: PathBuf,
    clock: Clock,
    preferred_port: u16,
    busy_timeout: Duration,
}

impl Environment {
    /// Resolves an environment from this process's variables, and from the
    /// `--store-path` this invocation carried.
    ///
    /// * `store` — see [`StoreLocation::resolve`]: `--store-path`,
    ///   `$STORYHOOK_STORE_PATH`, `$STORYHOOK_DATA_DIR/store.db`, then
    ///   `$XDG_DATA_HOME/storyhook/store.db`.
    /// * `state_home` — `$XDG_STATE_HOME/storyhook`, else
    ///   `~/.local/state/storyhook`. Deliberately *not* covered by
    ///   `STORYHOOK_DATA_DIR`, which names where the data is, so that pointing
    ///   storyhook at a synced data directory does not also start syncing its
    ///   runtime scratch. The per-store directories underneath it are what keep
    ///   two stores' daemons apart; see [`Self::daemon_state_dir`].
    /// * `daemon_addr` — `$STORYHOOK_DAEMON_ADDR`, else
    ///   [`default_daemon_port`] for this store. Port 0 asks the OS for one,
    ///   which is what the test harness sets: a suite that bound the production
    ///   port would fight the developer's own dashboard for it.
    /// * `busy_timeout` — `$STORYHOOK_BUSY_TIMEOUT_MS`, else
    ///   [`DEFAULT_BUSY_TIMEOUT`].
    ///
    /// `store_flag` is `None` everywhere except `main`, and that is correct
    /// rather than an oversight: `main` publishes the flag it was given into
    /// `$STORYHOOK_STORE_PATH` before anything else resolves, so a caller that
    /// re-resolves an environment later — `story daemon status`, the TUI, the
    /// daemon this run spawns — reads the same answer from the variable. The
    /// parameter survives only so that the *origin* of the choice can be
    /// reported accurately where the choice was actually made.
    ///
    /// An unparseable `STORYHOOK_DAEMON_ADDR` or `STORYHOOK_BUSY_TIMEOUT_MS` is
    /// an error rather than a silent fallback: both name where a request goes
    /// and how long it waits, and a typo that quietly reverts to the default is
    /// a debugging session. `STORYHOOK_DAEMON_ADDR` is refused for a second
    /// reason too — an IP other than `127.0.0.1`, which parses fine and names
    /// an address this daemon will not bind (see [`parse_daemon_port`]).
    pub fn from_process(store_flag: Option<&Path>) -> Result<Self, AppError> {
        let home = env_path("HOME")
            .ok_or_else(|| AppError::Storage("could not determine home directory".to_string()))?;

        let store = StoreLocation::resolve(store_flag, &StoreVars::from_process(), &home)?;
        if is_test_build() && store.origin() == StoreOrigin::XdgDefault {
            return Err(AppError::Usage(TEST_BUILD_REFUSAL.to_string()));
        }

        let state_home = env_path("XDG_STATE_HOME")
            .map(|xdg| xdg.join("storyhook"))
            .unwrap_or_else(|| home.join(".local/state/storyhook"));

        let preferred_port = match env_string("STORYHOOK_DAEMON_ADDR") {
            Some(raw) => parse_daemon_port(&raw)?,
            None => default_daemon_port_for_store(&store, &home),
        };

        let busy_timeout = match env_string("STORYHOOK_BUSY_TIMEOUT_MS") {
            Some(raw) => Duration::from_millis(raw.parse().map_err(|e| {
                AppError::Usage(format!(
                    "STORYHOOK_BUSY_TIMEOUT_MS=`{raw}` is not a number of milliseconds: {e}"
                ))
            })?),
            None => DEFAULT_BUSY_TIMEOUT,
        };

        Ok(Environment {
            store,
            state_home,
            home,
            clock: Clock::System,
            preferred_port,
            busy_timeout,
        })
    }

    /// An environment rooted at `home`, with XDG's own layout beneath it.
    ///
    /// The constructor for tests and for the fixtures that build them: it takes
    /// the one directory everything else hangs off, so a caller cannot
    /// accidentally isolate three of the four paths. The daemon address is
    /// loopback port 0 — never [`DEFAULT_DAEMON_PORT`], which the developer's
    /// own dashboard is probably holding.
    pub fn at(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Environment {
            store: StoreLocation::for_home(&home),
            state_home: home.join(".local/state/storyhook"),
            home,
            clock: Clock::System,
            preferred_port: 0,
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }

    /// Pins the clock, so that everything derived from "now" is comparable.
    #[must_use]
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Sets the port the daemon prefers to bind.
    #[must_use]
    pub fn daemon_port(mut self, port: u16) -> Self {
        self.preferred_port = port;
        self
    }

    /// Sets how long a writer waits for another process's write lock.
    #[must_use]
    pub fn busy_timeout(mut self, timeout: Duration) -> Self {
        self.busy_timeout = timeout;
        self
    }

    /// Points this environment at a different store.
    ///
    /// Everything derived from the store moves with it — the whole daemon state
    /// directory — which is what makes this the only honest way to build a
    /// second-store fixture. Assembling one field at a time is what produced a
    /// client and a daemon that disagreed in the first place.
    #[must_use]
    pub fn with_store(mut self, store: StoreLocation) -> Self {
        self.store = store;
        self
    }

    /// Which store this invocation is about, and how that was decided.
    pub fn store(&self) -> &StoreLocation {
        &self.store
    }

    /// Where regenerable state that should survive a reboot lives: the daemon's
    /// portfile, pidfile and log, and the backup snapshots.
    pub fn state_home(&self) -> &Path {
        &self.state_home
    }

    /// The user's home directory.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The variables a child that will run `story` needs in order to resolve
    /// **this** environment rather than its own process's (SH-633).
    ///
    /// Two facts, because two facts decide where a client looks for a daemon:
    /// the store (`STORYHOOK_STORE_PATH`) and the state home its runtime
    /// directory hangs under (`XDG_STATE_HOME`). Four spawn sites used to
    /// publish only the first. A child handed the store but not the state
    /// home resolved the latter from whatever `HOME` it had — the developer's
    /// real one, in every harness that cannot redirect `HOME` around `npm` or
    /// `cargo` (see `test_environment`'s scope on that parameter) — looked for
    /// the store's daemon under `~/.local/state/storyhook/daemons/<key>`,
    /// found nothing, and started a second daemon for the same store there.
    /// One store, one daemon (SH-113) held only as long as parent and child
    /// agreed about both halves of the path.
    ///
    /// `XDG_STATE_HOME` is the parent of [`Self::state_home`] rather than a
    /// stored field because both constructors build the state home as
    /// `<XDG_STATE_HOME>/storyhook` — [`Self::from_process`] from the
    /// variable or `$HOME/.local/state`, [`Self::at`] from the given home —
    /// and [`Self::with_store`] leaves it alone; there is no third way to
    /// make one. It is the lever the resolver already reads and the one
    /// `test_environment::TEST_ENVIRONMENT` names, so no second variable is
    /// invented for a fact that has one.
    ///
    /// Deliberately **not** here: `HOME`, which may only be redirected on a
    /// storyhook process and these children run `git` and `gh`; and
    /// `STORYHOOK_DAEMON_ADDR`, because a client finds a daemon by its
    /// portfile, never by the preferred port, and the harness owns that
    /// variable through `daemon_containment()`.
    ///
    /// Pass to `Command::envs` **after** any allowlist has cleared the
    /// child's environment, since `env_clear` discards what preceded it.
    pub fn child_vars(&self) -> Vec<(&'static str, PathBuf)> {
        let xdg_state_home = self
            .state_home
            .parent()
            .expect("a state home is <XDG_STATE_HOME>/storyhook and always has a parent")
            .to_path_buf();
        vec![
            ("STORYHOOK_STORE_PATH", self.store_path().to_path_buf()),
            ("XDG_STATE_HOME", xdg_state_home),
        ]
    }

    /// The current time, from this environment's clock.
    pub fn now(&self) -> String {
        self.clock.now()
    }

    /// The port a daemon should try first.
    ///
    /// The preferred port keeps a bookmarked dashboard URL working across
    /// restarts; falling back to an OS-assigned one
    /// ([`crate::daemon::lifecycle::bind_preferred`]) means a daemon can always
    /// start, even when something else holds it. A test harness sets this to 0,
    /// so a suite never contends for the port a developer's own dashboard is
    /// using.
    ///
    /// A port and not a [`SocketAddr`] (SH-253): the address is not this
    /// value's to name. `crate::daemon::serve::bind_listeners` binds
    /// `127.0.0.1`, and the tailnet interface beside it comes from an identity
    /// `tailscale` reports — so an address carried this far could only ever be
    /// discarded on arrival, which is exactly what used to happen.
    pub fn preferred_port(&self) -> u16 {
        self.preferred_port
    }

    /// How long a writer waits for another process's write lock.
    pub fn busy_timeout_value(&self) -> Duration {
        self.busy_timeout
    }

    /// The store's database file, canonicalized.
    pub fn store_path(&self) -> &Path {
        self.store.path()
    }

    /// `~/.storyhook` — where storyhook's *previous* global state lives.
    ///
    /// The dashboard's repo registry, its pid file and its log were all put
    /// here, outside XDG's layout, before locked decision 6 moved storyhook's
    /// global state to the data and state homes. Nothing new is written here;
    /// the directory is read so that what is already in it can be adopted, and
    /// it is **never deleted** — it is the only copy of state a user may still
    /// want.
    ///
    /// Deliberately hung off `home` rather than `data_home`: that path says
    /// where storyhook's data should go, and this one answers where it
    /// historically went. A test that redirects the former still has to be able
    /// to prove nothing touched the latter.
    pub fn legacy_global_dir(&self) -> PathBuf {
        self.home.join(".storyhook")
    }

    /// This store's daemon's runtime directory: `daemons/<key>/` under the
    /// state home.
    ///
    /// **This is the whole mechanism.** Because every one of the daemon's files
    /// hangs off the store's own key, one store has exactly one daemon by
    /// construction — and a client that named store A cannot find, dial, or
    /// stand down the daemon holding store B, because it never looks in that
    /// directory at all.
    pub fn daemon_state_dir(&self) -> PathBuf {
        self.daemons_dir().join(self.store.key())
    }

    /// The directory every store's [`Self::daemon_state_dir`] hangs under:
    /// `daemons/` in the state home. One entry per store this state home has
    /// ever served, and the only place `story daemon gc` looks (SH-638).
    pub fn daemons_dir(&self) -> PathBuf {
        self.state_home.join("daemons")
    }

    /// The daemon's portfile: `{pid, port, version, protocol, exe, exe_mtime,
    /// started_at, token, store_path}`, mode 0600.
    pub fn daemon_file(&self) -> PathBuf {
        self.daemon_state_dir().join(runtime_file::PORTFILE)
    }

    /// The file the daemon holds a lock on for its whole life. Holding the lock
    /// *is* the liveness signal, so this is not merely where a pid is written.
    pub fn daemon_pidfile(&self) -> PathBuf {
        self.daemon_state_dir().join(runtime_file::PIDFILE)
    }

    /// The daemon-owned process-group registry used by forced shutdown.
    ///
    /// Entries carry native process-incarnation identities, so a stale PID can
    /// never authorize signaling an unrelated process. The file is atomic
    /// JSON, mode 0600, and absent when no child process group is active.
    pub fn daemon_processes(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.processes.json")
    }

    /// The lock a client takes while it decides to spawn a daemon, held through
    /// the spawn and the child's portfile write.
    pub fn daemon_spawn_lock(&self) -> PathBuf {
        self.daemon_state_dir().join(runtime_file::SPAWN_LOCK)
    }

    /// Where a client that held the spawn lock leaves the verdict of its
    /// attempt, for the clients that were queued behind it.
    ///
    /// A **sibling** of [`Self::daemon_spawn_lock`] and never that file itself:
    /// the `flock` lives on the inode, so publishing by `rename` over the lock
    /// would hand the next waiter a different inode from the one the holder is
    /// locking, and the mutual exclusion would silently stop existing.
    ///
    /// Distinct from [`Self::daemon_failure`], which the *daemon* writes about
    /// itself on its way out. This one is written by a **client** about an
    /// attempt, and the two answer different questions: "why did the daemon
    /// stop" against "what happened when somebody last tried to start one"
    /// (SH-143).
    pub fn daemon_attempt(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.attempt.json")
    }

    /// Where a daemon started in the background writes its diagnostics.
    pub fn daemon_log(&self) -> PathBuf {
        self.daemon_state_dir().join(runtime_file::LOG)
    }

    /// The previous daemon's [`Self::daemon_log`], one spawn old (SH-287).
    ///
    /// [`crate::daemon::lifecycle::spawn_child`] renames the live log here
    /// before truncating a fresh one — a crash's whole stderr would otherwise
    /// be destroyed by the very next spawn, which is usually the very next
    /// `story` command. [`crate::daemon::crash::harvest`] is the only reader,
    /// and only once: it moves whatever it finds into
    /// [`Self::crash_logs_dir`] before this file can be rotated away again.
    pub fn daemon_log_rotated(&self) -> PathBuf {
        self.daemon_state_dir().join(runtime_file::LOG_ROTATED)
    }

    /// Where this store's daemon persists its finished
    /// [`crate::api::dispatch::DispatchRecord`]s across a restart (SH-232).
    ///
    /// Before this, `DispatchRegistry` was in-memory only: a `--auto`
    /// dispatch is the one kind nobody is necessarily watching when it
    /// finishes, and a daemon restart between the finish and the next look
    /// forgot it entirely. A JSON array of the *same bounded set*
    /// `DispatchRegistry` already keeps in memory — rewritten whole on
    /// every finish rather than appended, so this file never outgrows what
    /// a running daemon would show anyway, and never needs its own
    /// eviction logic to duplicate the in-memory one.
    ///
    /// Deliberately never records a dispatch still
    /// [`Running`](crate::api::dispatch::DispatchState::Running) when this
    /// daemon exits: that dispatch's child was launched into its own
    /// process group ([`crate::api::dispatch`]'s own doc), so it outlives
    /// the daemon as an orphan, and nothing is left to observe its outcome
    /// and write it down. Absent from history is the honest answer there,
    /// not a wrong one manufactured to fill the gap.
    ///
    /// Same shape as [`Self::daemon_current`] for the same reason: no
    /// schema version, because a parse failure (an older binary's format)
    /// reads as empty rather than as an error — losing dispatch history is
    /// never worse than a fresh daemon refusing to start over a file it no
    /// longer understands.
    pub fn dispatch_history(&self) -> PathBuf {
        self.daemon_state_dir().join("dispatch-history.json")
    }

    /// Where the daemon publishes everything it is currently serving.
    ///
    /// **The observable the wire does not have.** A daemon writes no bytes at
    /// all until a handler returns, so a client waiting on `/api/v1/invoke`
    /// cannot tell — from the socket — whether its command is running, queued
    /// behind somebody else's, or wedged. This file is how it finds out, and it
    /// is read without asking the daemon anything, which matters because a
    /// wedged handler answers nothing (SH-144).
    ///
    /// A JSON array of `CurrentRequest`, in arrival order — not the single
    /// object it was before SH-173's dispatch pool made more than one request
    /// legitimately in flight at once. Each entry is added by
    /// [`crate::daemon::lifecycle::Entry::name`] when a request's envelope has
    /// parsed and removed when its answer is ready, so the *set* changes
    /// exactly when the daemon **finishes something**. That is the whole
    /// signal: a client's deadline resets on every change to the set, which is
    /// what lets queueing be unbounded while a client's own served time is
    /// not. Absent and empty mean the same thing — nothing in flight — and the
    /// file is removed rather than written as `[]` so that meaning survives
    /// unchanged from when it held at most one bare object.
    ///
    /// The third of three files a client reads about a daemon rather than from
    /// it, beside [`Self::daemon_attempt`] (a *client* on a start attempt) and
    /// [`Self::daemon_failure`] (the *daemon* on its way out). This one is the
    /// daemon on what it is doing right now.
    ///
    /// **No schema version, deliberately.** `lifecycle::usable` trusts a daemon
    /// only when it is running this very binary, so a client can only ever read
    /// a record its own build wrote. A version field here would be a second
    /// answer to a question the binary check has already settled.
    ///
    /// **One writer of the set.** A raw write from two callers at once would
    /// let the second clobber the first — the defect concurrent dispatch would
    /// otherwise reintroduce — so every write goes through one
    /// [`crate::daemon::lifecycle::InFlight`], which is what makes widening
    /// this record to more than one entry safe. If a future need widens what
    /// gets published here again, the fix is to widen this one write site
    /// rather than to add a second one: two writers of one record is how the
    /// two ends come to disagree about what it means.
    pub fn daemon_current(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.current.json")
    }

    /// Where a daemon records work it abandoned rather than finished.
    ///
    /// Written only by `story daemon stop --force` (what it was still
    /// serving when the grace period ran out) and by the next daemon's own
    /// start (what [`Self::daemon_current`] still named when nothing could
    /// legitimately still be running — the previous daemon did not exit
    /// normally). A JSON array, in the order entries were abandoned; absent
    /// when nothing has been. `story doctor` reads it to tell a user their
    /// tracker may hold work whose outcome nobody confirmed, and
    /// `story doctor abandoned` is how they review and clear it.
    pub fn daemon_abandoned(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.abandoned.json")
    }

    /// Where a dying daemon's panic hook records what it caught (SH-287).
    ///
    /// Written by [`crate::daemon::crash::install_panic_hook`] the instant a
    /// panic unwinds, from inside the panic hook itself — best-effort, mode
    /// 0600, the same discipline [`Self::daemon_abandoned`] uses. Consumed and
    /// cleared by [`crate::daemon::crash::harvest`] at the *next* daemon's
    /// start, which is the only reader: nothing about a panic is actionable
    /// until there is a fresh daemon to act on it.
    pub fn daemon_panics(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.panics.json")
    }

    /// The ledger of every crash this store's daemon has noticed, oldest
    /// first (SH-287).
    ///
    /// Written by [`crate::daemon::crash::harvest`], the same moment it
    /// consumes [`Self::daemon_panics`] — a crash a daemon caused but a human
    /// has not yet reviewed. `story doctor crashes` is how they review and
    /// clear it, the same shape [`Self::daemon_abandoned`] and
    /// `story doctor abandoned` already established.
    pub fn daemon_crashes(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.crashes.json")
    }

    /// Where a crash's own preserved `daemon.log` is kept, one file per
    /// crash, named by [`crate::daemon::crash::CrashRecord::id`] (SH-287).
    ///
    /// A crash's log would otherwise be destroyed by the very next daemon
    /// spawn, which truncates [`Self::daemon_log`] unconditionally
    /// ([`crate::daemon::lifecycle::spawn_child`]) — this directory is where
    /// [`crate::daemon::crash::harvest`] moves it before that happens.
    /// Pruned to the newest few the same way [`Self::backups_dir`] is.
    pub fn crash_logs_dir(&self) -> PathBuf {
        self.daemon_state_dir().join("crashes")
    }

    /// Where a daemon that fails to start records *why*, for the client that
    /// started it.
    ///
    /// A separate file from [`Self::daemon_log`] rather than a tail of it, and
    /// the reason is that the log is a human stream with many writers — the
    /// dashboard's banner, backup notices, tailnet warnings, and every event
    /// hook that runs inside the daemon. Reading it back would make a
    /// diagnostic channel into an undeclared machine interface, which is
    /// exactly what the wire envelope exists to avoid. This file holds one
    /// [`crate::error::WireError`] and nothing else, so the client reconstructs
    /// the daemon's error rather than parsing its prose (SH-114).
    pub fn daemon_failure(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.failure.json")
    }

    /// Where a daemon built before the store keyed its own state published its
    /// portfile.
    ///
    /// Read once, on the first run after an upgrade, and never written. See
    /// [`crate::daemon::lifecycle`] for what is done with it: a daemon still
    /// serving the default store from here has to be stood down rather than
    /// run beside, because its pidfile is not the one a new daemon claims.
    pub fn legacy_daemon_file(&self) -> PathBuf {
        self.state_home.join("daemon.json")
    }

    /// Where the daily `VACUUM INTO` snapshots go.
    ///
    /// Keyed for a named store and unkeyed for the default one, and the
    /// asymmetry is deliberate. `run_if_due` prunes to a week, so a scratch
    /// store's daemon sharing this directory would delete the real store's
    /// backup history — a second store must therefore have its own. The default
    /// store keeps the path its snapshots are already at, because moving them
    /// would be a migration whose only reward is symmetry.
    pub fn backups_dir(&self) -> PathBuf {
        self.per_store_state().join("backups")
    }

    /// Where a rare, maintainer-invoked, store-wide rewrite — `story project
    /// set-prefix` is the first — takes its own safety snapshot before
    /// writing.
    ///
    /// **Deliberately not [`Self::backups_dir`].** `daemon::backup::prune`
    /// scans that one directory and deletes down to the newest
    /// [`crate::daemon::backup::RETAIN`], matching on filename alone; a
    /// snapshot dropped there could be swept by the very next daemon restart,
    /// gone before the operator it protects ever needed it (SH-135 is this
    /// failure happening to a hand-taken backup, which is what a snapshot
    /// left unprotected here would repeat automatically). Nothing prunes this
    /// directory — an operation rare enough to want its own copy of the whole
    /// store is rare enough that unbounded growth here is a maintainer's
    /// problem to notice, not a daemon's to solve unasked.
    pub fn maintenance_backups_dir(&self) -> PathBuf {
        self.per_store_state().join("maintenance/backups")
    }

    /// Where state that belongs to *this* store, and must survive its daemon,
    /// lives.
    fn per_store_state(&self) -> PathBuf {
        if self.store.is_default() {
            self.state_home.clone()
        } else {
            self.daemon_state_dir()
        }
    }
}

/// The only IP a storyhook daemon binds on its own account.
///
/// `crate::daemon::serve::bind_listeners` binds this literal; the tailnet
/// interface beside it is bound from an identity `tailscale` reports, never
/// from anything a variable named. So this is the whole of what
/// `$STORYHOOK_DAEMON_ADDR` may legitimately name.
const DAEMON_BIND_IP: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

/// Parses `$STORYHOOK_DAEMON_ADDR` into the port it names, refusing an IP this
/// daemon will not bind.
///
/// # Why the IP is checked rather than ignored (SH-253)
///
/// The variable parses as a full [`SocketAddr`] but only its port has ever been
/// used — [`Environment::preferred_port`] is all that survives resolution, and the
/// bind is a hardcoded `127.0.0.1`. So `STORYHOOK_DAEMON_ADDR=0.0.0.0:3456`
/// was *accepted*, silently ignored, and left an operator believing they had
/// widened a bind that never moved. Half a parsed value quietly discarded is a
/// defect on its own; since SH-250 it is a security-shaped one, because
/// "which interface did this arrive on" is now what decides whether a read
/// needs a credential at all.
///
/// # Exactly `127.0.0.1`, not any loopback address
///
/// Deliberately not `is_loopback()`, which would also admit `127.0.0.2` and
/// `::1`. Those are real, distinct sockets that this daemon never binds, so
/// accepting one would preserve the very defect this check exists to remove —
/// in a smaller box. `crate::api::http::host_is_loopback` accepts `::1` as a
/// `Host`, which makes the mismatch actively confusing rather than merely
/// unused.
///
/// The refusal fires in [`Environment::from_process`], which runs at the top of
/// *every* `story` invocation rather than only at daemon startup. That is why
/// the message names the fix three times over — the accepted spelling, the flag
/// that sets a port on its own, and the tailnet interface that answers the need
/// a wider bind would have been reached for.
fn parse_daemon_port(raw: &str) -> Result<u16, AppError> {
    let addr: SocketAddr = raw.parse().map_err(|e| {
        AppError::Usage(format!(
            "STORYHOOK_DAEMON_ADDR=`{raw}` is not an address: {e}"
        ))
    })?;
    if addr.ip() != DAEMON_BIND_IP {
        return Err(AppError::Usage(format!(
            "STORYHOOK_DAEMON_ADDR=`{raw}` names an IP the daemon does not bind. \
             It binds {DAEMON_BIND_IP} — and, on a machine with a tailnet, that \
             Tailscale interface as well — so only `{DAEMON_BIND_IP}:PORT` is \
             accepted here, and `--port PORT` sets the port without naming an \
             address at all. To reach the dashboard from another machine, use \
             the tailnet address the daemon already binds for you: a wider bind \
             would expose a full-privilege API to the local network."
        )));
    }
    Ok(addr.port())
}

/// The port a daemon prefers when nothing names one.
///
/// The store designated as the home default keeps [`DEFAULT_DAEMON_PORT`], so
/// a bookmarked dashboard URL survives restarts and the launchd agent needs no
/// change. Any other store binds port 0 and publishes what the kernel gave it,
/// which is what makes two isolated stores unable to collide on a port — and
/// therefore what makes a parallel test suite safe without the harness having
/// to choose ports.
#[must_use]
pub fn default_daemon_port(is_default_store: bool) -> u16 {
    if is_default_store {
        DEFAULT_DAEMON_PORT
    } else {
        0
    }
}

/// The port a resolved store prefers when no address overrides it.
fn default_daemon_port_for_store(store: &StoreLocation, home: &Path) -> u16 {
    default_daemon_port(store.is_default_for_home(home))
}

/// Where the store lives on this machine when nothing names one.
///
/// Deliberately *not* routed through [`Environment`]: `story store new` has to
/// know the default path in order to refuse it, and a test build refuses to
/// resolve an `Environment` on the default store at all — which is exactly the
/// build that most needs to be able to create a scratch store somewhere else.
pub fn default_store_path() -> Result<PathBuf, AppError> {
    let home = env_path("HOME")
        .ok_or_else(|| AppError::Storage("could not determine home directory".to_string()))?;
    store_location::default_store_path_for(env_path("XDG_DATA_HOME").as_deref(), &home)
}

impl StoreVars {
    /// The store-naming variables, as this process has them.
    fn from_process() -> Self {
        StoreVars {
            store_path: env_path("STORYHOOK_STORE_PATH"),
            data_dir: env_path("STORYHOOK_DATA_DIR"),
            xdg_data_home: env_path("XDG_DATA_HOME"),
        }
    }
}

/// Whether this binary was built for testing.
///
/// The `fault-injection` feature is the sentinel, and it is an exact one rather
/// than an approximation. `cargo build` and `cargo build --release` do not
/// enable it; `cargo test` does, because `storyhook-test-support` — a
/// dev-dependency of this package — depends on `storyhook` *with* the feature,
/// and cargo's feature resolver keeps dev-dependency features out of non-test
/// builds. Every binary a test run can reach therefore answers `true` here, and
/// no binary a user can install does.
///
/// Reusing the store's crash-injection switch for this is deliberate: a second
/// sentinel would be a second thing to keep true, and the two questions —
/// "may this build stop the world mid-commit?" and "may this build write to a
/// real tracker?" — have the same answer for the same reason.
///
/// Building a release *with* `--features fault-injection` makes that build
/// answer `true` and so subject to the same refusal. That is the correct
/// reading: a binary carrying live crash points is a test binary however it was
/// produced.
#[must_use]
pub const fn is_test_build() -> bool {
    cfg!(feature = "fault-injection")
}

/// What a test build says when it is asked to pick a data directory itself.
///
/// The whole message is here rather than built at the raise site so the test
/// that pins it can compare against the constant — which, since SH-528,
/// something finally does: `storyhook_test_support`'s fault-capability probe
/// asks this binary whether it is a test build by running it and looking for
/// exactly this string, because [`is_test_build`] *is* the `fault-injection`
/// feature and there is no other way to ask an artifact on disk what it can
/// do. Rendered verbatim (`#[error("{0}")]`), so a `contains` against the
/// constant is an exact comparison rather than a fuzzy one.
/// Both readings are answered on purpose. Someone running the suite by hand
/// needs to be told about `make test`; someone who typed
/// `./target/debug/story list` after a `cargo test` needs to be told that the
/// binary in front of them is not the one they meant, because the refusal
/// otherwise reads as storyhook being broken.
///
/// # Why this checks `origin()` and not [`StoreLocation::is_default`] (SH-95)
///
/// It looks like a hole: naming the default store explicitly — `--store-path
/// ~/.local/share/storyhook/store.db`, or either variable pointed there — gets
/// `origin() != XdgDefault` and is not refused. Tried the obvious fix
/// (`store.is_default()`, "did we end up at the real store" rather than "did
/// anything name it") and reverted it: `is_default()` is relative to whatever
/// `HOME` this process has, and [`crate::env`]'s own primary test harness
/// (`TestEnv`, in `storyhook-test-support`) *deliberately* builds a fake `HOME`
/// and points every store-naming variable at that fake home's own
/// default-shaped subdirectory — `home.join(".local/share/storyhook")` — so
/// that a fixture behaves like a real layout while staying disposable. That
/// makes `is_default()` true for essentially every correctly isolated fixture
/// in the suite, indistinguishable by path alone from the real hazard. Nothing
/// available here can tell "a fake `HOME` mirroring the default layout" apart
/// from "the real `HOME` at the default layout", because both are answered
/// from the same two inputs. Switching the predicate refused the harness's own
/// conformance test (`the_harness_always_names_a_data_directory`) and, through
/// it, every fixture built the same way — caught by running the suite, not by
/// review. Accepted as a known, narrow gap rather than chased further: it
/// requires an explicit override that happens to spell out the literal real
/// default path while also running with an unmodified real `HOME`, which is a
/// much smaller target than the "nothing named it" case this guard exists for.
pub const TEST_BUILD_REFUSAL: &str = "refusing to guess where the store lives: this binary carries \
     the `fault-injection` feature, which `cargo test` enables and `cargo build` does not, so it \
     is a test build — and with nothing naming a store it would fall back to the real \
     ~/.local/share/storyhook. Run the suite with `make test`, which exports an isolated \
     STORYHOOK_DATA_DIR; name a store yourself with --store-path, $STORYHOOK_STORE_PATH or \
     $STORYHOOK_DATA_DIR; or, if you meant to *use* this binary, rebuild it with `cargo build`.";

/// One environment variable as a path, ignoring an empty value.
///
/// An empty `XDG_DATA_HOME` is what a shell leaves behind when an export is
/// unset the careless way, and joining `storyhook` onto it would silently make a
/// *relative* path — a store in whatever directory the process happened to
/// start in.
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// One environment variable as a non-empty string.
fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- `$STORYHOOK_DAEMON_ADDR`'s IP means something now (SH-253) ---

    /// The spelling every harness, the README and the launchd agent use.
    #[test]
    fn the_daemon_address_accepts_the_address_the_daemon_binds() {
        assert_eq!(
            parse_daemon_port("127.0.0.1:0").expect("the harness spelling"),
            0
        );
        assert_eq!(
            parse_daemon_port("127.0.0.1:3456").expect("the bookmarked spelling"),
            DEFAULT_DAEMON_PORT
        );
    }

    /// The defect SH-253 names: an IP that parsed, was thrown away, and left
    /// the operator believing they had moved a bind that never moved.
    ///
    /// `127.0.0.2` and `::1` are in this list on purpose. Both *are* loopback,
    /// so an `is_loopback()` check would admit them — and both are distinct
    /// sockets this daemon never binds, which is the same silent discard in a
    /// smaller box. Only the literal the daemon binds is accepted.
    #[test]
    fn the_daemon_address_refuses_every_ip_the_daemon_will_not_bind() {
        for raw in [
            "0.0.0.0:3456",
            "192.168.1.5:3456",
            "100.64.0.1:3456",
            "[::1]:3456",
            "[::]:3456",
            "127.0.0.2:3456",
        ] {
            let error = parse_daemon_port(raw)
                .expect_err("an IP the daemon does not bind must not be accepted and ignored");
            assert!(
                matches!(error, AppError::Usage(_)),
                "{raw} is the operator's mistake to fix, not a storage failure"
            );
        }
    }

    /// The refusal fires on every `story` invocation, not just daemon startup,
    /// so a message that only says "no" reads as a total outage. It has to name
    /// the way out — all three of them.
    #[test]
    fn the_refusal_names_the_accepted_spelling_the_flag_and_the_tailnet() {
        let AppError::Usage(message) =
            parse_daemon_port("0.0.0.0:3456").expect_err("a wildcard bind is refused")
        else {
            panic!("a misconfigured variable is a usage error");
        };
        assert!(
            message.contains("127.0.0.1:PORT"),
            "the accepted spelling: {message}"
        );
        assert!(message.contains("--port"), "the flag: {message}");
        assert!(message.contains("tailnet"), "the remote answer: {message}");
    }

    /// The older refusal still stands, and still says which variable it means.
    #[test]
    fn the_daemon_address_still_refuses_something_that_is_not_an_address() {
        let error = parse_daemon_port("localhost:3456")
            .expect_err("a hostname is not a SocketAddr, and never was");
        let AppError::Usage(message) = error else {
            panic!("a misconfigured variable is a usage error");
        };
        assert!(message.contains("STORYHOOK_DAEMON_ADDR"), "{message}");
        assert!(message.contains("is not an address"), "{message}");
    }

    #[test]
    fn an_empty_variable_is_ignored_rather_than_joined_onto() {
        // Joining onto "" yields a relative path, which would put a user's
        // whole tracker wherever the process happened to start.
        assert_eq!(env_path("STORYHOOK_ENV_TEST_ABSENT"), None);
        assert_eq!(env_string("STORYHOOK_ENV_TEST_ABSENT"), None);
    }

    #[test]
    fn the_legacy_global_directory_is_not_where_new_state_goes() {
        // The adoption path reads one and writes the other; if they ever
        // resolved to the same directory, "never delete the legacy state" and
        // "this is our data directory" would be claims about one place.
        let env = Environment::at("/tmp/storyhook-env-test");
        assert_ne!(
            Some(env.legacy_global_dir().as_path()),
            env.store_path().parent()
        );
        assert_ne!(env.legacy_global_dir(), env.state_home());
        assert!(env.legacy_global_dir().ends_with(".storyhook"));
    }

    #[test]
    fn the_store_is_a_file_named_store_db() {
        let env = Environment::at("/tmp/storyhook-env-test");
        assert_eq!(env.store_path().file_name().unwrap(), "store.db");
        assert!(env.store_path().is_absolute());
    }

    /// The mechanism, as an assertion: every file a daemon touches hangs off
    /// its store's key, so two stores cannot reach each other's.
    #[test]
    fn the_daemons_runtime_files_all_live_in_this_stores_own_directory() {
        let env = Environment::at("/tmp/storyhook-env-test");
        let keyed = env.daemon_state_dir();
        for path in [
            env.daemon_file(),
            env.daemon_pidfile(),
            env.daemon_spawn_lock(),
            env.daemon_attempt(),
            env.daemon_failure(),
            env.daemon_log(),
            env.dispatch_history(),
        ] {
            assert_eq!(
                path.parent(),
                Some(keyed.as_path()),
                "{} must live in this store's own daemon directory",
                path.display()
            );
        }
        assert!(keyed.starts_with(env.state_home()));
        assert!(keyed.ends_with(env.store().key()));
    }

    /// What a child is told is what the table says a child reads (SH-633).
    ///
    /// Derived in both directions over `test_environment::TEST_ENVIRONMENT`:
    /// every name [`Environment::child_vars`] publishes is a table parameter,
    /// and for a root-relative one its value is exactly what the table renders
    /// for the same root — so the in-process constructor and the process-level
    /// isolation cannot disagree about where a child's store or state home is.
    /// The table is the definition of what a storyhook process resolves from;
    /// a name published here that the table does not know is a fact the child
    /// would ignore, and a value that differs is a child resolving somewhere
    /// its parent is not looking.
    #[test]
    fn child_vars_agree_with_the_test_environment_table() {
        use test_environment::{Disposition, Scope, TEST_ENVIRONMENT};

        let root = Path::new("/private/tmp/storyhook-env-test-root");
        let env = Environment::at(root.join("home"));
        let published = env.child_vars();
        assert!(!published.is_empty(), "a child is told something");

        let rendered = test_environment::resolve(root, 0, Scope::Anywhere);
        for (name, value) in &published {
            let parameter = TEST_ENVIRONMENT
                .iter()
                .find(|parameter| parameter.name == *name)
                .unwrap_or_else(|| {
                    panic!("child_vars publishes `{name}`, which the test-environment table does not know")
                });
            assert!(
                matches!(parameter.disposition, Disposition::Root(_)),
                "`{name}` is not root-relative in the table, so its value cannot be derived from an environment"
            );
            let expected = rendered
                .iter()
                .find(|setting| setting.name == *name)
                .and_then(|setting| setting.value.clone())
                .expect("a root-relative parameter has a value");
            assert_eq!(
                value.as_os_str(),
                expected.as_os_str(),
                "`{name}`: child_vars says {} and the table says {}",
                value.display(),
                Path::new(&expected).display()
            );
        }
    }

    /// A child is told the state home its parent resolved, not only the store
    /// (SH-633) — and nothing else that another mechanism already owns.
    #[test]
    fn child_vars_name_the_store_and_the_state_home_and_nothing_else() {
        let env = Environment::at("/private/tmp/storyhook-env-test");
        let names: Vec<&str> = env.child_vars().iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["STORYHOOK_STORE_PATH", "XDG_STATE_HOME"]);

        // The store moves with `with_store`; the state home does not, which is
        // what `with_store`'s own doc promises.
        let other = StoreLocation::for_home(Path::new("/private/tmp/storyhook-env-other"));
        let moved = env.clone().with_store(other.clone());
        let vars: std::collections::BTreeMap<&str, PathBuf> =
            moved.child_vars().into_iter().collect();
        assert_eq!(vars["STORYHOOK_STORE_PATH"], other.path());
        assert_eq!(vars["XDG_STATE_HOME"], env.state_home().parent().unwrap());
    }

    /// Two stores must not be able to name one another's runtime files — which
    /// is the property that makes "client and daemon disagree about the store"
    /// unrepresentable rather than detected.
    #[test]
    fn two_stores_share_no_daemon_state_at_all() {
        let one = Environment::at("/tmp/storyhook-env-test-one");
        let two = Environment::at("/tmp/storyhook-env-test-two");
        assert_ne!(one.store_path(), two.store_path());
        for (a, b) in [
            (one.daemon_state_dir(), two.daemon_state_dir()),
            (one.daemon_file(), two.daemon_file()),
            (one.daemon_pidfile(), two.daemon_pidfile()),
            (one.daemon_spawn_lock(), two.daemon_spawn_lock()),
            (one.daemon_attempt(), two.daemon_attempt()),
            (one.daemon_failure(), two.daemon_failure()),
            (one.daemon_log(), two.daemon_log()),
            (one.dispatch_history(), two.dispatch_history()),
        ] {
            assert_ne!(a, b, "{} is shared between two stores", a.display());
        }
    }

    /// `run_if_due` prunes to a week, so a scratch store's daemon sharing the
    /// default store's backup directory would delete real backup history.
    #[test]
    fn a_named_store_does_not_back_up_into_the_default_stores_directory() {
        let default = Environment::at("/tmp/storyhook-env-test");
        assert_eq!(
            default.backups_dir().parent(),
            Some(default.state_home()),
            "the default store keeps the path its snapshots are already at"
        );

        let named = default.clone().with_store(
            StoreLocation::resolve(
                Some(Path::new("/private/tmp/storyhook-env-test/named.db")),
                &StoreVars::default(),
                Path::new("/tmp/storyhook-env-test"),
            )
            .expect("resolving a named store"),
        );
        assert!(!named.store().is_default());
        assert_ne!(named.backups_dir(), default.backups_dir());
        assert_ne!(
            named.maintenance_backups_dir(),
            default.maintenance_backups_dir()
        );
        assert!(named.backups_dir().starts_with(named.daemon_state_dir()));
    }

    /// The property `set_prefix`'s safety snapshot depends on: its directory
    /// is not the one `daemon::backup::prune` scans, so a daily prune sweep
    /// can never delete it out from under an operator who has not looked at
    /// it yet — the SH-135 failure mode, closed for this caller by construction
    /// rather than by remembering to name the backup so it sorts differently.
    #[test]
    fn the_maintenance_backup_directory_is_not_the_pruned_one() {
        let env = Environment::at("/tmp/storyhook-env-test");
        assert_ne!(env.maintenance_backups_dir(), env.backups_dir());
    }

    /// The default store keeps the port a bookmarked dashboard names; anything
    /// else takes whatever the kernel gives it, so two isolated stores cannot
    /// collide.
    #[test]
    fn only_the_default_store_prefers_the_production_port() {
        assert_eq!(default_daemon_port(true), DEFAULT_DAEMON_PORT);
        assert_eq!(default_daemon_port(false), 0);
    }

    /// `$XDG_DATA_HOME` changes this process's default, but not the store a
    /// daemon launched later without that environment will serve. The relocated
    /// store must therefore take an OS-assigned port instead of competing with
    /// the login-time default for the stable dashboard port (SH-428).
    #[test]
    fn an_xdg_default_store_does_not_prefer_the_production_port() {
        let home = Path::new("/tmp/storyhook-sh-428-home");
        let login_default = StoreLocation::for_home(home);
        let explicitly_named_login_default =
            StoreLocation::resolve(Some(login_default.path()), &StoreVars::default(), home)
                .expect("resolving the login default through an explicit path");
        let store = StoreLocation::resolve(
            None,
            &StoreVars {
                xdg_data_home: Some(home.join("xdg")),
                ..StoreVars::default()
            },
            home,
        )
        .expect("resolving a store through XDG_DATA_HOME");

        assert!(
            store.is_default(),
            "the regression requires the old predicate to be fooled"
        );
        assert!(!store.is_default_for_home(home));
        assert_eq!(default_daemon_port_for_store(&store, home), 0);
        assert!(login_default.is_default_for_home(home));
        assert_eq!(
            default_daemon_port_for_store(&login_default, home),
            DEFAULT_DAEMON_PORT
        );
        assert_eq!(
            default_daemon_port_for_store(&explicitly_named_login_default, home),
            DEFAULT_DAEMON_PORT
        );
    }

    /// The upgrade path reads one and writes the other. If they were ever the
    /// same file, standing the old daemon down would mean deleting the new
    /// one's portfile.
    #[test]
    fn the_legacy_portfile_is_not_where_this_build_publishes_one() {
        let env = Environment::at("/tmp/storyhook-env-test");
        assert_ne!(env.legacy_daemon_file(), env.daemon_file());
        assert_eq!(env.legacy_daemon_file().parent(), Some(env.state_home()));
    }

    /// A fixture that binds the production port would fight the developer's own
    /// dashboard for it — and win, sometimes.
    #[test]
    fn a_constructed_environment_never_prefers_the_production_port() {
        let env = Environment::at("/tmp/storyhook-env-test");
        assert_eq!(env.preferred_port(), 0);
    }

    #[test]
    fn the_clock_is_pinnable() {
        let env = Environment::at("/tmp/storyhook-env-test")
            .clock(Clock::Fixed("2020-01-01T00:00:00Z".to_string()));
        assert_eq!(env.now(), "2020-01-01T00:00:00Z");
    }

    #[test]
    fn the_busy_timeout_is_configurable_and_defaults_to_five_seconds() {
        let env = Environment::at("/tmp/storyhook-env-test");
        assert_eq!(env.busy_timeout_value(), DEFAULT_BUSY_TIMEOUT);
        let quick = env.busy_timeout(Duration::from_millis(250));
        assert_eq!(quick.busy_timeout_value(), Duration::from_millis(250));
    }
}
