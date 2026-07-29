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

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::AppError;
use crate::service::Clock;

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
    data_home: PathBuf,
    state_home: PathBuf,
    home: PathBuf,
    clock: Clock,
    daemon_addr: SocketAddr,
    busy_timeout: Duration,
}

impl Environment {
    /// Resolves an environment from this process's variables.
    ///
    /// * `data_home` — `$STORYHOOK_DATA_DIR`, else `$XDG_DATA_HOME/storyhook`,
    ///   else `~/.local/share/storyhook`.
    /// * `state_home` — `$XDG_STATE_HOME/storyhook`, else
    ///   `~/.local/state/storyhook`. Deliberately *not* covered by
    ///   `STORYHOOK_DATA_DIR`, which names where the data is, so that pointing
    ///   storyhook at a synced data directory does not also start syncing its
    ///   runtime scratch.
    /// * `daemon_addr` — `$STORYHOOK_DAEMON_ADDR`, else loopback on
    ///   [`DEFAULT_DAEMON_PORT`]. Port 0 asks the OS for one, which is what the
    ///   test harness sets: a suite that bound the production port would fight
    ///   the developer's own dashboard for it.
    /// * `busy_timeout` — `$STORYHOOK_BUSY_TIMEOUT_MS`, else
    ///   [`DEFAULT_BUSY_TIMEOUT`].
    ///
    /// An unparseable `STORYHOOK_DAEMON_ADDR` or `STORYHOOK_BUSY_TIMEOUT_MS` is
    /// an error rather than a silent fallback: both name where a request goes
    /// and how long it waits, and a typo that quietly reverts to the default is
    /// a debugging session.
    pub fn from_process() -> Result<Self, AppError> {
        let home = env_path("HOME")
            .ok_or_else(|| AppError::Storage("could not determine home directory".to_string()))?;

        let named_data_home = env_path("STORYHOOK_DATA_DIR");
        if is_test_build() && named_data_home.is_none() {
            return Err(AppError::Usage(TEST_BUILD_REFUSAL.to_string()));
        }
        let data_home = named_data_home
            .or_else(|| env_path("XDG_DATA_HOME").map(|xdg| xdg.join("storyhook")))
            .unwrap_or_else(|| home.join(".local/share/storyhook"));

        let state_home = env_path("XDG_STATE_HOME")
            .map(|xdg| xdg.join("storyhook"))
            .unwrap_or_else(|| home.join(".local/state/storyhook"));

        let daemon_addr = match env_string("STORYHOOK_DAEMON_ADDR") {
            Some(raw) => raw.parse().map_err(|e| {
                AppError::Usage(format!(
                    "STORYHOOK_DAEMON_ADDR=`{raw}` is not an address: {e}"
                ))
            })?,
            None => SocketAddr::from(([127, 0, 0, 1], DEFAULT_DAEMON_PORT)),
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
            data_home,
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
            data_home: home.join(".local/share/storyhook"),
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

    /// Where the store and everything derived from it lives.
    pub fn data_home(&self) -> &Path {
        &self.data_home
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

    /// The store's database file.
    pub fn store_path(&self) -> PathBuf {
        self.data_home.join("store.db")
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

    /// The daemon's portfile: `{pid, port, version, protocol, exe, exe_mtime,
    /// started_at, token}`, mode 0600.
    pub fn daemon_file(&self) -> PathBuf {
        self.state_home.join("daemon.json")
    }

    /// The file the daemon holds a lock on for its whole life. Holding the lock
    /// *is* the liveness signal, so this is not merely where a pid is written.
    pub fn daemon_pidfile(&self) -> PathBuf {
        self.state_home.join("daemon.pid")
    }

    /// The lock a client takes while it decides to spawn a daemon, held through
    /// the spawn and the child's portfile write.
    pub fn daemon_spawn_lock(&self) -> PathBuf {
        self.state_home.join("daemon.spawn.lock")
    }

    /// Where a daemon started in the background writes its diagnostics.
    pub fn daemon_log(&self) -> PathBuf {
        self.state_home.join("daemon.log")
    }

    /// Where the daily `VACUUM INTO` snapshots go.
    pub fn backups_dir(&self) -> PathBuf {
        self.state_home.join("backups")
    }

    /// Where github-sync keeps its pre-write backups.
    pub fn github_backups_dir(&self) -> PathBuf {
        self.state_home.join("github-sync/backups")
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
const TEST_BUILD_REFUSAL: &str = "refusing to guess where the store lives: this binary carries \
     the `fault-injection` feature, which `cargo test` enables and `cargo build` does not, so it \
     is a test build — and with $STORYHOOK_DATA_DIR unset it would fall back to the real \
     ~/.local/share/storyhook. Run the suite with `make test`, which exports an isolated \
     STORYHOOK_DATA_DIR; set STORYHOOK_DATA_DIR yourself; or, if you meant to *use* this binary, \
     rebuild it with `cargo build`.";

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
        assert_ne!(env.legacy_global_dir(), env.data_home());
        assert_ne!(env.legacy_global_dir(), env.state_home());
        assert!(env.legacy_global_dir().ends_with(".storyhook"));
    }

    #[test]
    fn the_store_lives_inside_the_data_home() {
        let env = Environment::at("/tmp/storyhook-env-test");
        assert_eq!(env.store_path().parent(), Some(env.data_home()));
        assert_eq!(env.store_path().file_name().unwrap(), "store.db");
    }

    #[test]
    fn the_daemons_runtime_files_all_live_in_the_state_home() {
        let env = Environment::at("/tmp/storyhook-env-test");
        for path in [
            env.daemon_file(),
            env.daemon_pidfile(),
            env.daemon_spawn_lock(),
            env.daemon_log(),
            env.backups_dir(),
        ] {
            assert_eq!(
                path.parent(),
                Some(env.state_home()),
                "{} must live in the state home, not beside the data",
                path.display()
            );
        }
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
