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
mod store_location;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::AppError;
use crate::service::Clock;

pub use store_location::{StoreLocation, StoreOrigin, StoreVars, canonical_ish};

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
    daemon_addr: SocketAddr,
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
    ///   [`default_daemon_addr`] for this store. Port 0 asks the OS for one,
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
    /// a debugging session.
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

        let daemon_addr = match env_string("STORYHOOK_DAEMON_ADDR") {
            Some(raw) => raw.parse().map_err(|e| {
                AppError::Usage(format!(
                    "STORYHOOK_DAEMON_ADDR=`{raw}` is not an address: {e}"
                ))
            })?,
            None => default_daemon_addr(store.is_default()),
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
            daemon_addr,
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
            daemon_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }

    /// Pins the clock, so that everything derived from "now" is comparable.
    #[must_use]
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Sets the address the daemon binds and clients dial.
    #[must_use]
    pub fn daemon_addr(mut self, addr: SocketAddr) -> Self {
        self.daemon_addr = addr;
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

    /// The current time, from this environment's clock.
    pub fn now(&self) -> String {
        self.clock.now()
    }

    /// The address the daemon prefers to bind, and the one a client dials
    /// before consulting the portfile.
    pub fn preferred_daemon_addr(&self) -> SocketAddr {
        self.daemon_addr
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
        self.state_home.join("daemons").join(self.store.key())
    }

    /// The daemon's portfile: `{pid, port, version, protocol, exe, exe_mtime,
    /// started_at, token, store_path}`, mode 0600.
    pub fn daemon_file(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.json")
    }

    /// The file the daemon holds a lock on for its whole life. Holding the lock
    /// *is* the liveness signal, so this is not merely where a pid is written.
    pub fn daemon_pidfile(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.pid")
    }

    /// The lock a client takes while it decides to spawn a daemon, held through
    /// the spawn and the child's portfile write.
    pub fn daemon_spawn_lock(&self) -> PathBuf {
        self.daemon_state_dir().join("daemon.spawn.lock")
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
        self.daemon_state_dir().join("daemon.log")
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

    /// Where github-sync keeps its pre-write backups. Sited like
    /// [`Self::backups_dir`], and for the same reason.
    pub fn github_backups_dir(&self) -> PathBuf {
        self.per_store_state().join("github-sync/backups")
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

/// The address a daemon prefers when nothing names one.
///
/// The **default** store keeps [`DEFAULT_DAEMON_PORT`], so a bookmarked
/// dashboard URL survives restarts and the launchd agent needs no change. Any
/// other store binds port 0 and publishes what the kernel gave it, which is what
/// makes two isolated stores unable to collide on a port — and therefore what
/// makes a parallel test suite safe without the harness having to choose ports.
#[must_use]
pub fn default_daemon_addr(is_default_store: bool) -> SocketAddr {
    let port = if is_default_store {
        DEFAULT_DAEMON_PORT
    } else {
        0
    };
    SocketAddr::from(([127, 0, 0, 1], port))
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
/// that pins it can compare against the constant.
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
const TEST_BUILD_REFUSAL: &str = "refusing to guess where the store lives: this binary carries \
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
        assert_ne!(named.github_backups_dir(), default.github_backups_dir());
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
        assert_ne!(env.maintenance_backups_dir(), env.github_backups_dir());
    }

    /// The default store keeps the port a bookmarked dashboard names; anything
    /// else takes whatever the kernel gives it, so two isolated stores cannot
    /// collide.
    #[test]
    fn only_the_default_store_prefers_the_production_port() {
        assert_eq!(default_daemon_addr(true).port(), DEFAULT_DAEMON_PORT);
        assert_eq!(default_daemon_addr(false).port(), 0);
        assert!(default_daemon_addr(true).ip().is_loopback());
        assert!(default_daemon_addr(false).ip().is_loopback());
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
        assert_eq!(env.preferred_daemon_addr().port(), 0);
        assert!(env.preferred_daemon_addr().ip().is_loopback());
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
