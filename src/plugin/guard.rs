//! The fourth guard on `path_identity::build_dir`: an uninstalled build never
//! manages a provider's plugin registration.
//!
//! [`crate::migration_guard`] (SH-404, SH-630) stops a binary still in its
//! build directory from advancing the default store's *schema*,
//! [`crate::daemon::install_guard`] (SH-411) from becoming the machine's
//! *login agent*, and [`crate::daemon::seat_guard`] (SH-634) from becoming the
//! default store's *daemon*. Nothing stopped the same binary from running
//! `story plugin install|uninstall|reinstall` against the provider
//! configuration under whatever `HOME` it happened to have.
//!
//! The incident (SH-760): `tests/invoker_seam.rs` drives a roster of
//! project-less verbs *in-process* through `StoreInvoker`, and one of them is
//! `plugin uninstall claude`. A test binary's process keeps the developer's
//! own `HOME` and `PATH` — the harness redirects them on `story` *children*,
//! which an in-process call never is — so every run of that test executed the
//! real `claude plugin uninstall story@storyhook`, swept the real plugin cache,
//! and, under a bare `cargo test` with no `STORYHOOK_DATA_DIR`, removed the
//! install receipt as well. `story doctor install` then read the machine as
//! never installed. It had been happening on every gate run since the roster
//! was written; the loss was attributed to the provider each time.
//!
//! # Two facts, one rule
//!
//! Refused when [`OVERRIDE_VAR`] is unset and either:
//!
//! 1. **This is a test build** — it carries the `fault-injection` feature,
//!    which `cargo test` enables and `cargo build` does not
//!    ([`crate::env::is_test_build`]). Every binary a test run can reach
//!    answers `true`, *including a copy made outside the build directory*:
//!    the incident is a test binary whatever directory it was copied to, so
//!    this clause is the one that fires for it, and it fires without needing
//!    to know where the executable is.
//! 2. **The executable is still where cargo wrote it** — inside the directory
//!    `build.rs` stamped ([`crate::path_identity::build_dir`]), the fact the
//!    caller cannot rewrite. A `cargo build` binary carries no feature, so this
//!    is the clause that covers `./target/debug/story plugin install claude`
//!    typed in a checkout.
//!
//! A binary that is neither is installed — `make install`, `story update` and
//! `cargo install` all copy out — and is permitted outright. The override
//! permits either clause: it is the operator's statement that the home this
//! process sees is theirs to change, and the install receipt records that the
//! statement was made (`plugin::receipt`), which is what keeps a deliberate
//! uninstall quiet in `story doctor install` while one nobody asked for is
//! flagged.
//!
//! # Where it sits
//!
//! In `plugin::install` and `plugin::uninstall` after the target is parsed —
//! an unknown target is a usage error before anything else is consulted — and
//! before `preflight_provider`, the marketplace projection, the managed-path
//! manifest, any provider call or any file write; and at the top of
//! `plugin::reinstall::run`, before `plan()` reads anything, so a refused
//! reinstall is one refusal rather than one folded per provider into the
//! report. The verb executes in the daemon for a CLI client and in the test
//! binary for an in-process call, and the guard runs in whichever process
//! executes it: the plugin module is the choke point, not the CLI. The seat
//! guard keeps daemon and client one build, so the identity read is the same
//! from either side; the override, though, is read from the *daemon's*
//! environment, and the refusal says so.
//!
//! # Why not the account's home
//!
//! The precise hazard is "the home this process sees is the developer's", and
//! comparing `HOME` with the passwd entry would name it exactly. It was
//! rejected because the refusal could then never be proven end to end: the
//! only process that satisfies the predicate is one aimed at the real home,
//! and a test whose failure mode is destroying the developer's registration is
//! the defect this module exists to close. The build facts are provable from
//! an isolated home on both sides — `tests/plugin_guard.rs` refuses a test
//! binary and an installed copy of it, and the override is the control.

use std::path::{Path, PathBuf};

use crate::error::AppError;
use crate::path_identity;

/// The environment variable that deliberately bypasses this guard.
pub const OVERRIDE_VAR: &str = "STORYHOOK_ALLOW_UNINSTALLED_PLUGIN_INSTALL";

/// The `story plugin` verb being guarded, for the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Install,
    Uninstall,
    Reinstall,
}

impl Verb {
    /// The word on the command line.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
            Self::Reinstall => "reinstall",
        }
    }
}

/// How this process came to exist, as far as installation is concerned.
///
/// Also what the install receipt records about the actor
/// (`plugin::receipt`), so the doctor can later tell a refusal that was
/// bypassed from an uninstall the operator ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    /// Neither clause below applies: the binary was copied out of its build
    /// directory and carries no test feature.
    Installed,
    /// Still inside the directory `build.rs` stamped.
    Checkout,
    /// Carries the `fault-injection` feature: a `cargo test` artifact,
    /// wherever it sits.
    TestBuild,
}

impl Build {
    /// The receipt's spelling, and the doctor's.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Checkout => "checkout",
            Self::TestBuild => "test",
        }
    }

    /// The inverse of [`Self::token`]; `None` for a word this build never
    /// wrote.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "installed" => Some(Self::Installed),
            "checkout" => Some(Self::Checkout),
            "test" => Some(Self::TestBuild),
            _ => None,
        }
    }

    /// Classifies a process from the two facts, test feature first: a test
    /// build is refused whether or not it has left its build directory.
    #[must_use]
    pub fn classify(is_test_build: bool, exe: Option<&Path>, build_dir: Option<&Path>) -> Self {
        if is_test_build {
            return Self::TestBuild;
        }
        match (exe, build_dir) {
            (Some(exe), Some(dir)) if path_identity::is_inside_build_dir(exe, dir) => {
                Self::Checkout
            }
            _ => Self::Installed,
        }
    }
}

/// The facts the guard decides on, already resolved.
///
/// Plain values, so [`decide`] does no I/O and the whole truth table is a
/// table of literals in a test — the split [`crate::migration_guard`] draws,
/// for the reason stated there. [`gather`] is the only place that touches the
/// process environment.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// The verb being run.
    pub verb: Verb,
    /// Its provider, when it takes one.
    pub target: Option<&'static str>,
    /// This process's own executable, canonicalized. `None` only if the
    /// platform could not report it at all, which fails the build-directory
    /// clause open (as the migration guard does) but not the test-build one:
    /// a test build knows what it is without knowing where it is.
    pub current_exe: Option<PathBuf>,
    /// The directory cargo wrote this binary into, canonicalized.
    pub build_dir: Option<PathBuf>,
    /// Whether this binary carries the `fault-injection` feature.
    pub is_test_build: bool,
    /// Whether [`OVERRIDE_VAR`] is set.
    pub override_set: bool,
}

/// Why a permitted verb was permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permit {
    /// The binary is installed; nothing was at risk.
    Installed,
    /// The binary is uninstalled and the operator said so.
    Overridden,
}

/// Refuses an uninstalled build the verb unless the override is set.
///
/// `Ok` carries why; `Err` carries the full refusal message.
pub fn decide(inputs: &Inputs) -> Result<Permit, String> {
    let build = Build::classify(
        inputs.is_test_build,
        inputs.current_exe.as_deref(),
        inputs.build_dir.as_deref(),
    );
    if build == Build::Installed {
        return Ok(Permit::Installed);
    }
    if inputs.override_set {
        return Ok(Permit::Overridden);
    }
    Err(refusal(inputs, build))
}

/// The refusal: names the verb, the binary, which clause fired, and every
/// way through. Never `story update` (SH-405's dead end).
fn refusal(inputs: &Inputs, build: Build) -> String {
    let command = match inputs.target {
        Some(target) => format!("story plugin {} {target}", inputs.verb.token()),
        None => format!("story plugin {}", inputs.verb.token()),
    };
    let binary = inputs.current_exe.as_ref().map_or_else(
        || "this binary".to_string(),
        |exe| format!("this binary ({})", exe.display()),
    );
    let why = match build {
        Build::TestBuild => "is a test build — it carries the `fault-injection` feature, which \
             `cargo test` enables and `cargo build` does not"
            .to_string(),
        Build::Checkout => format!(
            "is still where cargo built it ({}) — it has not been installed, whatever $PATH says",
            inputs.build_dir.as_ref().map_or_else(
                || "its build directory".to_string(),
                |dir| dir.display().to_string()
            )
        ),
        Build::Installed => unreachable!("an installed build is never refused"),
    };
    format!(
        "refusing `{command}`: {binary} {why}. It would register, remove or replace the storyhook \
         plugin in the provider configuration under this process's HOME, which is how a test \
         suite once removed the real Claude Code registration on every run. Nothing has been \
         changed.\n\n\
         If you meant to manage the plugin, install this build first (`make install`, which \
         reinstalls the plugin for every provider that has it registered) and run the installed \
         `story`. If you meant to run this build once against a home you own, set \
         {OVERRIDE_VAR}=1 in the environment of the process that runs the verb — for a `story` \
         command that is the daemon, so export it and `story daemon restart` first — and run the \
         command again; the install receipt will record that the override was used."
    )
}

/// Resolves the real inputs [`decide`] needs, for the production callers.
///
/// Reads the process environment and this process's own identity; everything
/// past this point is pure.
#[must_use]
pub fn gather(verb: Verb, target: Option<&'static str>) -> Inputs {
    Inputs {
        verb,
        target,
        current_exe: path_identity::running_exe().map(|exe| exe.canonical),
        build_dir: path_identity::build_dir(),
        is_test_build: crate::env::is_test_build(),
        override_set: std::env::var_os(OVERRIDE_VAR).is_some(),
    }
}

/// The one door the plugin verbs call before they touch anything: gathers,
/// decides, and renders a refusal as the usage failure it is (exit 2, like
/// the migration and seat guards').
pub fn check(verb: Verb, target: Option<&'static str>) -> Result<Permit, AppError> {
    decide(&gather(verb, target)).map_err(AppError::Usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The incident's shape: a test binary in `deps/`, inside the stamp.
    fn test_binary() -> Inputs {
        Inputs {
            verb: Verb::Uninstall,
            target: Some("claude"),
            current_exe: Some(PathBuf::from(
                "/home/dev/repo/target/debug/deps/invoker_seam-1a2b",
            )),
            build_dir: Some(PathBuf::from("/home/dev/repo/target/debug")),
            is_test_build: true,
            override_set: false,
        }
    }

    /// `cargo build`'s binary, run from the checkout.
    fn checkout_binary() -> Inputs {
        Inputs {
            verb: Verb::Install,
            target: Some("claude"),
            current_exe: Some(PathBuf::from("/home/dev/repo/target/debug/story")),
            build_dir: Some(PathBuf::from("/home/dev/repo/target/debug")),
            is_test_build: false,
            override_set: false,
        }
    }

    /// What `make install` produces: copied out, no test feature.
    fn installed_binary() -> Inputs {
        Inputs {
            verb: Verb::Reinstall,
            target: None,
            current_exe: Some(PathBuf::from("/home/dev/.local/bin/story")),
            build_dir: Some(PathBuf::from("/home/dev/repo/target/debug")),
            is_test_build: false,
            override_set: false,
        }
    }

    #[test]
    fn the_incident_is_refused_and_the_message_names_every_way_out() {
        let error = decide(&test_binary()).expect_err("a test binary must be refused");
        assert!(
            error.starts_with("refusing `story plugin uninstall claude`"),
            "{error}"
        );
        assert!(
            error.contains("/home/dev/repo/target/debug/deps/invoker_seam-1a2b"),
            "{error}"
        );
        assert!(error.contains("test build"), "{error}");
        assert!(error.contains("make install"), "{error}");
        assert!(error.contains(OVERRIDE_VAR), "{error}");
        assert!(error.contains("story daemon restart"), "{error}");
        assert!(error.contains("Nothing has been changed"), "{error}");
        assert!(
            !error.contains("story update"),
            "SH-405's dead end: {error}"
        );
    }

    /// A test build is refused wherever it sits: copied out of the build
    /// directory is exactly what `installed_copy()` does, and the copy is
    /// still the incident.
    #[test]
    fn a_test_build_copied_out_of_its_build_directory_is_still_refused() {
        let mut inputs = test_binary();
        inputs.current_exe = Some(PathBuf::from("/tmp/installed-story/story"));
        let error = decide(&inputs).expect_err("must refuse");
        assert!(error.contains("test build"), "{error}");
        assert!(!error.contains("still where cargo built it"), "{error}");
    }

    /// The test-build clause needs no executable path at all.
    #[test]
    fn a_test_build_that_cannot_name_itself_is_still_refused() {
        let mut inputs = test_binary();
        inputs.current_exe = None;
        inputs.build_dir = None;
        let error = decide(&inputs).expect_err("must refuse");
        assert!(error.contains("this binary is a test build"), "{error}");
    }

    #[test]
    fn a_checkout_binary_is_refused_by_its_build_directory() {
        let error = decide(&checkout_binary()).expect_err("must refuse");
        assert!(
            error.starts_with("refusing `story plugin install claude`"),
            "{error}"
        );
        assert!(
            error.contains("still where cargo built it (/home/dev/repo/target/debug)"),
            "{error}"
        );
        assert!(
            error.contains("/home/dev/repo/target/debug/story"),
            "{error}"
        );
        assert!(!error.contains("test build"), "{error}");
    }

    /// `…/target/debug-old/story` is not inside `…/target/debug`: whole
    /// components, as `is_inside_build_dir` promises.
    #[test]
    fn a_sibling_directory_of_the_stamp_is_not_inside_it() {
        let mut inputs = checkout_binary();
        inputs.current_exe = Some(PathBuf::from("/home/dev/repo/target/debug-old/story"));
        assert_eq!(decide(&inputs), Ok(Permit::Installed));
    }

    #[test]
    fn the_installed_binary_is_permitted_with_or_without_the_override() {
        assert_eq!(decide(&installed_binary()), Ok(Permit::Installed));
        let mut inputs = installed_binary();
        inputs.override_set = true;
        assert_eq!(
            decide(&inputs),
            Ok(Permit::Installed),
            "an installed binary was never at risk; the override changes nothing about it"
        );
    }

    /// A `cargo build` binary with no stamp fails the build-directory clause
    /// open, as the migration guard does: nothing is provably at risk.
    #[test]
    fn a_non_test_build_with_no_stamp_or_no_executable_is_permitted() {
        let mut inputs = checkout_binary();
        inputs.build_dir = None;
        assert_eq!(decide(&inputs), Ok(Permit::Installed));
        let mut inputs = checkout_binary();
        inputs.current_exe = None;
        assert_eq!(decide(&inputs), Ok(Permit::Installed));
    }

    #[test]
    fn the_override_permits_both_uninstalled_shapes_and_says_so() {
        for mut inputs in [test_binary(), checkout_binary()] {
            inputs.override_set = true;
            assert_eq!(decide(&inputs), Ok(Permit::Overridden), "{inputs:?}");
        }
    }

    #[test]
    fn a_reinstall_refusal_names_the_verb_without_a_target() {
        let mut inputs = test_binary();
        inputs.verb = Verb::Reinstall;
        inputs.target = None;
        let error = decide(&inputs).expect_err("must refuse");
        assert!(
            error.starts_with("refusing `story plugin reinstall`:"),
            "{error}"
        );
    }

    #[test]
    fn build_tokens_round_trip_and_reject_strangers() {
        for build in [Build::Installed, Build::Checkout, Build::TestBuild] {
            assert_eq!(Build::parse(build.token()), Some(build));
        }
        assert_eq!(Build::parse("release"), None);
        assert_eq!(Build::parse(""), None);
    }

    /// This test binary is the incident's shape for real: the guard must
    /// refuse it in-process, which is the whole point of the module.
    #[test]
    fn this_very_test_binary_is_refused() {
        let inputs = gather(Verb::Uninstall, Some("claude"));
        assert!(inputs.is_test_build, "cargo test builds carry the feature");
        assert!(
            inputs.current_exe.is_some(),
            "a supported target reports its own executable"
        );
        if inputs.override_set {
            // A developer's exported override would disarm the assertion
            // below, which is exactly what the environment table clears it
            // for; say so rather than pass vacuously.
            panic!(
                "{OVERRIDE_VAR} is set in this test's environment; unset it or run under `make test`"
            );
        }
        let error = decide(&inputs).expect_err("the test binary must be refused");
        assert!(error.contains("test build"), "{error}");
    }
}
