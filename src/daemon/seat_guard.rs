//! The third guard on `path_identity::build_dir`: an uninstalled build may
//! not become the default store's daemon.
//!
//! [`crate::migration_guard`] (SH-404, SH-630) stops a binary still in its
//! build directory from advancing the default store's *schema*, and
//! [`crate::daemon::install_guard`] (SH-411) stops it from becoming the
//! machine's *login agent*. Between them sat the path both incidents actually
//! travelled: `lifecycle::spawn_locked` treats any live daemon that is not this
//! exact build as "not ours", asks it to stand down, and spawns the **client's
//! own binary** as the replacement. On 2026-09-09 a `PATH=target/debug:$PATH
//! story list` from a checkout replaced the installed daemon on the production
//! store with a worktree debug build; the next installed `story` replaced it
//! back; four custodians in one hour, each restart re-running daemon-start
//! reconcile (SH-634). SH-630 stopped the migration that build then ran.
//! Nothing stopped the seating.
//!
//! Why the seat matters even with the migration refused: the daemon runs the
//! dispatch engine and the centralized verifier and hands `STORY_BIN`, its own
//! `current_exe`, to every child it spawns. A worktree build seated on the
//! production store runs production automation on whatever that worktree holds
//! next, and the next `cargo build` rewrites the binary underneath it. That is
//! the hazard `make scratch` was built for (SH-531: "`./target/debug/story
//! list`, typed in a worktree, resolves the REAL store and port 3456"), observed
//! rather than described.
//!
//! # Two clauses, one guard
//!
//! Both refuse only an **uninstalled** binary — one whose canonical executable
//! sits inside the directory `build.rs` stamped
//! ([`crate::path_identity::build_dir`]), the fact the caller cannot rewrite —
//! and both permit when [`OVERRIDE_VAR`] is set.
//!
//! 1. **Replacing an incumbent.** The store is the default one
//!    ([`Inputs::is_default`]) and a live daemon of another build already holds
//!    it. Refused *before* the shutdown request, so the installed daemon keeps
//!    serving: a refusal one line lower would stop the daemon and then decline
//!    to replace it, leaving the store with no custodian at all (SH-411's
//!    "gate one line too low", whose only observable is what is left
//!    afterwards). This is the incident.
//!
//! 2. **Taking an empty seat.** Nothing named the store
//!    ([`Inputs::named_explicitly`] is false — `StoreOrigin::XdgDefault`), so
//!    this is the bare invocation from a checkout, and it is refused whether or
//!    not a daemon is running. The harm is the same whether the seat was empty
//!    or occupied, and a guard that only covered the occupied case would make
//!    `./target/debug/story list` work when the installed daemon happened to be
//!    down and refuse when it happened to be up — a worse contract than a
//!    consistent refusal that names the way through.
//!
//! **Not** "refuse the stand-down but talk to the incumbent when the protocol
//! matches", which the story that filed this offered as the alternative. Since
//! SH-114 every command's service runs inside the daemon and the client
//! contributes parsing and rendering only, so a worktree build talking to the
//! installed daemon would exercise none of the worktree's changes while looking
//! exactly as though it had — a wrong answer with the confident shape of a
//! right one. It would also reverse `lifecycle::PROTOCOL`'s invariant that a
//! daemon and a client from different builds never talk.
//!
//! # Why the two clauses key on different facts
//!
//! Clause 1 keys on [`crate::env::StoreLocation::is_default`], exactly as the
//! migration guard does and for its reasons: it is true for an
//! `$XDG_DATA_HOME`-relocated real store, and it is what lets a fixture under a
//! fake `HOME` prove the refusal end to end. It is also true for nearly every
//! fixture in this repository's suite (`TestEnv` mirrors the default layout —
//! `src/env/mod.rs`'s `TEST_BUILD_REFUSAL` doc records a guard keyed on it
//! alone being reverted for that reason), which is why clause 1 is narrowed by
//! the **incumbent**: it fires only when a live daemon of another build is
//! already there, and every harness in this tree runs one build per store.
//!
//! Clause 2 has no incumbent to narrow it, so it keys on the store's *origin*
//! for `TEST_BUILD_REFUSAL`'s reasons: every harness names its store, and only
//! the bare invocation does not. A test build cannot reach clause 2 through a
//! subprocess — `Environment::from_process` refuses an `XdgDefault` origin
//! first — but `Environment::at` builds exactly that origin in-process, which
//! is how `tests/seat_guard.rs` drives the real `spawn_locked` into it.
//!
//! # Where it sits, and why that is a contract
//!
//! After the spawn lock is taken and the under-lock "use what is there" and
//! "adopt a peer's verdict" early returns, before any side effect — the stale
//! login-agent note, the shutdown request, the legacy stand-down, the spawn —
//! and **outside** the closure whose outcome `publish_attempt` records: the
//! refusal is about *this client's* build, and an installed client waiting
//! behind the lock must never adopt it. After the lock rather than before it,
//! because `tests/daemon_timeouts.rs` holds the spawn lock and calls
//! `lifecycle::ensure` in-process on an `Environment::at` — an uninstalled
//! binary on an `XdgDefault` origin — and must fail *at the lock*, naming it.
//! The consequence is accepted and stated: an uninstalled client behind a busy
//! holder waits up to `SPAWN_LOCK_DEADLINE` and is then refused. The guard makes
//! no control call and takes no wait, so the lock's bound arithmetic is
//! untouched.
//!
//! # Deliberate limits
//!
//! * A live daemon with an **unparseable** portfile is not an incumbent here;
//!   it also cannot be displaced, because the spawned child fails to claim the
//!   pidfile. Nothing is lost.
//! * A daemon at the **legacy** portfile (`stand_down_legacy_daemon`) is outside
//!   clause 1: telling whether one is live needs a `hello` round trip inside the
//!   lock, which the lock's bound forbids. The bare invocation still meets
//!   clause 2; a build that names the default store explicitly at its default
//!   location can stand a pre-key daemon down. Narrow, and stated.
//! * A symlinked "install" pointing into a build directory canonicalizes inside
//!   the stamp and meets clause 2 on every daemon start. Not a mechanism this
//!   tree ships; the refusal and `story daemon status` both name the way out.
//! * `story daemon stop` is not a seat and stays unguarded.
//!
//! The override is a variable rather than a flag, the opposite of the install
//! guard's choice and for its stated reason: the process that needs it is every
//! `story` command in a `make scratch` shell, not one command typed by a person.
//! `scripts/test-env.sh --uninstalled-build` re-arms it beside the migration
//! override, and that re-arm is load-bearing — a `cargo build` inside a scratch
//! shell changes the binary's mtime, so the next command meets a live daemon of
//! "another build" on a default-shaped store, which is clause 1.

use std::path::PathBuf;

use crate::daemon::lifecycle::{self, DaemonInfo};
use crate::env::{Environment, StoreOrigin};
use crate::error::AppError;
use crate::path_identity;

/// The environment variable that deliberately bypasses this guard.
pub const OVERRIDE_VAR: &str = "STORYHOOK_ALLOW_UNINSTALLED_DAEMON";

/// The live daemon of another build that a refusal names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incumbent {
    /// Its process id.
    pub pid: u32,
    /// The version it published.
    pub version: String,
    /// The executable it published.
    pub exe: PathBuf,
}

impl Incumbent {
    /// The daemon holding `env`'s store, if it is live, parseable, and **not**
    /// this build — the exact shape `spawn_locked` would otherwise stand down.
    ///
    /// A daemon of this build is never an incumbent: the caller has already
    /// found it unusable for some other reason (a store-key collision), and a
    /// refusal naming "another build" would be a lie.
    #[must_use]
    pub fn observe(env: &Environment) -> Option<Self> {
        let info = lifecycle::read_info(env)?;
        (lifecycle::is_live(env) && !info.is_this_binary()).then(|| Self::from(&info))
    }
}

impl From<&DaemonInfo> for Incumbent {
    fn from(info: &DaemonInfo) -> Self {
        Self {
            pid: info.pid,
            version: info.version.clone(),
            exe: info.exe.clone(),
        }
    }
}

/// The facts the guard decides on, already resolved.
///
/// Plain values, so [`decide`] does no I/O and the whole truth table is a
/// table of literals in a test — the split [`crate::migration_guard`] draws,
/// for the reason stated there. [`gather`] is the only place that touches the
/// process environment or the filesystem.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// The store a daemon would be started for.
    pub store_path: PathBuf,
    /// Whether that is the store this environment defaults to
    /// ([`crate::env::StoreLocation::is_default`]).
    pub is_default: bool,
    /// Whether anything named the store — `--store-path` or a variable —
    /// rather than it being resolved from `HOME` alone.
    pub named_explicitly: bool,
    /// A live daemon of another build already holding the store, if any.
    pub incumbent: Option<Incumbent>,
    /// This process's own executable, canonicalized. `None` only if the
    /// platform could not report it; treated like a missing stamp — nothing
    /// provably at risk, so the guard permits.
    pub current_exe: Option<PathBuf>,
    /// The directory cargo wrote this binary into, canonicalized — `build.rs`'s
    /// stamp — or `None` for a build that carried none.
    pub build_dir: Option<PathBuf>,
    /// Whether [`OVERRIDE_VAR`] is set.
    pub override_set: bool,
}

/// Why the guard refused.
///
/// An enum rather than a `String` because the two make different claims — one
/// names a daemon that was left alone, the other a seat that was left empty —
/// and a test asserting on prose could not tell them apart. Boxed by [`decide`]
/// because it carries four paths and the permit is a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Clause 1: a live daemon of another build holds the default store.
    ReplaceIncumbent {
        store_path: PathBuf,
        incumbent: Incumbent,
        running: PathBuf,
        build_dir: PathBuf,
    },
    /// Clause 2: nothing named the store, and this binary would have started
    /// its daemon.
    SeatEmpty {
        store_path: PathBuf,
        running: PathBuf,
        build_dir: PathBuf,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReplaceIncumbent {
                store_path,
                incumbent,
                running,
                build_dir,
            } => write!(
                f,
                "refusing to replace the storyhook daemon holding `{store}` (pid {pid}, storyhook \
                 {version}, {exe}): this binary ({running}) is still where cargo built it \
                 ({build_dir}) — it has not been installed, whatever $PATH says.\n\n\
                 A daemon started from a build directory runs this store's dispatch engine and \
                 verifier on whatever that directory holds next, and the next `cargo build` \
                 rewrites the binary underneath it. Nothing was stopped; the daemon you had is \
                 still serving.\n\n{ways_out}",
                store = store_path.display(),
                pid = incumbent.pid,
                version = incumbent.version,
                exe = incumbent.exe.display(),
                running = running.display(),
                build_dir = build_dir.display(),
                ways_out = ways_out(),
            ),
            Self::SeatEmpty {
                store_path,
                running,
                build_dir,
            } => write!(
                f,
                "refusing to start a storyhook daemon for `{store}`, the default store, from this \
                 binary ({running}): it is still where cargo built it ({build_dir}) — it has not \
                 been installed, and nothing named a store, so this is a checkout's build reaching \
                 your real tracker.\n\n\
                 A daemon started from a build directory runs this store's dispatch engine and \
                 verifier on whatever that directory holds next, and the next `cargo build` \
                 rewrites the binary underneath it. No daemon was started.\n\n{ways_out}",
                store = store_path.display(),
                running = running.display(),
                build_dir = build_dir.display(),
                ways_out = ways_out(),
            ),
        }
    }
}

/// The remedies both refusals end with. Never `story update` (SH-405): an
/// unreleased build has no release to update to.
fn ways_out() -> String {
    format!(
        "To try this build, use `make scratch` (a throwaway store with its own daemon) or name \
         a store with `--store-path`. To use it for real, install it first (`make install`, or \
         however this tree normally installs) and run the installed `story` — a copy outside \
         its build directory is what installing means here. To run this build against your \
         real store anyway, set {OVERRIDE_VAR}=1 and run the command again."
    )
}

/// Decides whether this binary may start a daemon for the store in `inputs`.
///
/// The rows are a conjunction read in order. A store that is not the default
/// one is the caller's deliberate choice and permits outright — nothing named
/// resolves to `XdgDefault`, so this also covers every explicitly named store
/// (see the module doc for the one it does not: the default store named at its
/// own location). Then the override, then the two facts whose absence permits
/// because nothing is provably at risk, then the build directory, then the two
/// clauses.
pub fn decide(inputs: &Inputs) -> Result<(), Box<Refusal>> {
    if !inputs.is_default {
        return Ok(());
    }
    if inputs.override_set {
        return Ok(());
    }
    let (Some(running), Some(build_dir)) = (&inputs.current_exe, &inputs.build_dir) else {
        return Ok(());
    };
    if !path_identity::is_inside_build_dir(running, build_dir) {
        return Ok(());
    }
    if let Some(incumbent) = &inputs.incumbent {
        return Err(Box::new(Refusal::ReplaceIncumbent {
            store_path: inputs.store_path.clone(),
            incumbent: incumbent.clone(),
            running: running.clone(),
            build_dir: build_dir.clone(),
        }));
    }
    if !inputs.named_explicitly {
        return Err(Box::new(Refusal::SeatEmpty {
            store_path: inputs.store_path.clone(),
            running: running.clone(),
            build_dir: build_dir.clone(),
        }));
    }
    Ok(())
}

/// Resolves the real inputs [`decide`] needs, for the production callers.
///
/// Reads the process environment, this process's own identity, and the
/// store's daemon files; everything past this point is pure.
#[must_use]
pub fn gather(env: &Environment, incumbent: Option<Incumbent>) -> Inputs {
    Inputs {
        store_path: env.store_path().to_path_buf(),
        is_default: env.store().is_default(),
        named_explicitly: env.store().origin() != StoreOrigin::XdgDefault,
        incumbent,
        current_exe: path_identity::running_exe().map(|exe| exe.canonical),
        build_dir: path_identity::build_dir(),
        override_set: std::env::var_os(OVERRIDE_VAR).is_some(),
    }
}

/// The one door `lifecycle` calls before it stands anything down or starts
/// anything: observes the incumbent, decides, and renders a refusal as the
/// usage failure it is (exit 2, like the migration guard's).
pub fn check(env: &Environment) -> Result<(), AppError> {
    decide(&gather(env, Incumbent::observe(env)))
        .map_err(|refusal| AppError::Usage(refusal.to_string()))
}

/// Whether the next command from this binary would be refused the seat `info`
/// currently holds — for `story daemon status`, which otherwise promises a
/// restart the guard would decline.
#[must_use]
pub fn would_refuse(env: &Environment, info: &DaemonInfo) -> bool {
    decide(&gather(env, Some(Incumbent::from(info)))).is_err()
}

/// The remedies, for a caller rendering its own sentence around them.
#[must_use]
pub fn remedies() -> String {
    ways_out()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under `/private/tmp`, not `$TMPDIR`: Spotlight indexes the latter.
    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-seat-guard-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    fn incumbent() -> Incumbent {
        Incumbent {
            pid: 4242,
            version: "2.4.3".to_string(),
            exe: PathBuf::from("/home/dev/.local/bin/story"),
        }
    }

    /// The SH-634 incident: a worktree's debug binary, the default store named
    /// through `$PATH`'s own prefix (so it reads as explicitly named — the
    /// harness shape), and the installed daemon live.
    fn incident() -> Inputs {
        Inputs {
            store_path: PathBuf::from("/home/dev/.local/share/storyhook/store.db"),
            is_default: true,
            named_explicitly: true,
            incumbent: Some(incumbent()),
            current_exe: Some(PathBuf::from("/home/dev/repo/target/debug/story")),
            build_dir: Some(PathBuf::from("/home/dev/repo/target/debug")),
            override_set: false,
        }
    }

    /// The SH-531 hazard: the bare invocation from a checkout, no daemon.
    fn bare_invocation() -> Inputs {
        Inputs {
            named_explicitly: false,
            incumbent: None,
            ..incident()
        }
    }

    #[test]
    fn the_incident_is_refused_and_names_the_daemon_left_alone() {
        let refusal = decide(&incident()).expect_err("must refuse");
        assert!(matches!(*refusal, Refusal::ReplaceIncumbent { .. }));
        let text = refusal.to_string();
        for needle in [
            "/home/dev/.local/share/storyhook/store.db",
            "pid 4242",
            "storyhook 2.4.3",
            "/home/dev/.local/bin/story",
            "/home/dev/repo/target/debug/story",
            "/home/dev/repo/target/debug",
            "Nothing was stopped",
            "make scratch",
            "--store-path",
            "make install",
            OVERRIDE_VAR,
        ] {
            assert!(text.contains(needle), "missing {needle:?} in: {text}");
        }
        assert!(!text.contains("story update"), "SH-405's dead end: {text}");
    }

    #[test]
    fn the_bare_invocation_is_refused_with_no_daemon_running() {
        let refusal = decide(&bare_invocation()).expect_err("must refuse");
        assert!(matches!(*refusal, Refusal::SeatEmpty { .. }));
        let text = refusal.to_string();
        for needle in [
            "No daemon was started",
            "make scratch",
            "--store-path",
            "make install",
            OVERRIDE_VAR,
        ] {
            assert!(text.contains(needle), "missing {needle:?} in: {text}");
        }
        assert!(!text.contains("story update"));
    }

    #[test]
    fn the_bare_invocation_with_an_incumbent_names_the_incumbent() {
        let inputs = Inputs {
            incumbent: Some(incumbent()),
            ..bare_invocation()
        };
        assert!(matches!(
            decide(&inputs).map_err(|refusal| *refusal),
            Err(Refusal::ReplaceIncumbent { .. })
        ));
    }

    #[test]
    fn an_installed_binary_replaces_an_incumbent() {
        let inputs = Inputs {
            current_exe: Some(PathBuf::from("/home/dev/.local/bin/story")),
            ..incident()
        };
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn an_installed_binary_takes_an_empty_seat() {
        let inputs = Inputs {
            current_exe: Some(PathBuf::from("/home/dev/.local/bin/story")),
            ..bare_invocation()
        };
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn an_explicitly_named_default_store_with_no_incumbent_is_permitted() {
        // The harness shape: `TestEnv` names the default-shaped store and
        // starts its own daemon from the uninstalled test binary.
        let inputs = Inputs {
            incumbent: None,
            ..incident()
        };
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn a_store_that_is_not_the_default_is_never_guarded() {
        let inputs = Inputs {
            store_path: PathBuf::from("/tmp/scratch/store.db"),
            is_default: false,
            ..incident()
        };
        assert_eq!(decide(&inputs), Ok(()));
        let inputs = Inputs {
            is_default: false,
            ..bare_invocation()
        };
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn the_override_permits_both_clauses() {
        for base in [incident(), bare_invocation()] {
            let inputs = Inputs {
                override_set: true,
                ..base
            };
            assert_eq!(decide(&inputs), Ok(()));
        }
    }

    #[test]
    fn a_binary_with_no_stamp_or_no_identity_permits() {
        for base in [incident(), bare_invocation()] {
            assert_eq!(
                decide(&Inputs {
                    build_dir: None,
                    ..base.clone()
                }),
                Ok(())
            );
            assert_eq!(
                decide(&Inputs {
                    current_exe: None,
                    ..base
                }),
                Ok(())
            );
        }
    }

    #[test]
    fn a_sibling_directory_of_the_build_directory_is_outside_it() {
        let inputs = Inputs {
            current_exe: Some(PathBuf::from("/home/dev/repo/target/debug-old/story")),
            ..incident()
        };
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn gather_reads_this_binary_and_the_process_environment() {
        // No daemon files under a bare scratch home: `observe` finds nothing,
        // and the rest of the inputs are this process's own facts.
        let dir = scratch();
        let env = Environment::at(dir.path());
        let inputs = gather(&env, Incumbent::observe(&env));
        assert!(inputs.is_default);
        assert!(
            !inputs.named_explicitly,
            "Environment::at resolves the store from HOME alone"
        );
        assert!(inputs.incumbent.is_none());
        assert_eq!(
            inputs.current_exe,
            path_identity::running_exe().map(|exe| exe.canonical)
        );
        assert_eq!(inputs.build_dir, path_identity::build_dir());
        assert_eq!(inputs.store_path, env.store_path());
    }

    #[test]
    fn a_daemon_of_this_build_is_never_an_incumbent() {
        // `observe` is what `spawn_locked` reaches once `usable` said no. It
        // must not manufacture an incumbent from a portfile this build wrote.
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert!(Incumbent::observe(&env).is_none());
    }
}
