//! The write-side counterpart to the SH-54 forward-compatibility gate.
//!
//! SH-54 protects the *read* side: a binary refuses a store a newer one wrote,
//! rather than misreading it. Nothing protected the *write* side — a binary
//! built from source, run with no store override, would resolve the real data
//! home and apply every pending migration to it, one-way, with no prompt and no
//! notice. `storyhook::env::is_test_build` fences `cargo test` builds out of
//! the real data home; a `cargo build` binary carries no such fence at all.
//!
//! The incident this closes (SH-404): a worktree's debug binary, carrying a
//! migration merged to `main` but shipped in no release, advanced the real
//! store's schema from 16 to 17. The installed release binary understood only
//! 16, so every `story` command failed at daemon start until a build from
//! `main` was installed by hand. No data was damaged — only the schema stamp
//! moved — but the tracker was down, and the failure gave no warning on the
//! way in, only a refusal on the way out.
//!
//! # The predicate — two facts, and why the first was not enough
//!
//! The lookups live in [`crate::path_identity`], which SH-411 extracted when a
//! second guard needed the same question asked — and reached the opposite
//! conclusion from a *missing* answer, which is why the two share the facts
//! and not the judgement.
//!
//! **Has this binary left the directory cargo wrote it into?** `build.rs`
//! stamps that directory ([`crate::path_identity::build_dir`]), and a binary
//! still inside it is refused before anything else is consulted. That is what
//! "installed" means in this tree and in every mechanism it ships: `make
//! install`, `story update` and `cargo install` all *copy out*, so the
//! sanctioned recovery from the SH-404 incident — `make install` from `main`,
//! carrying the unreleased migration on purpose — satisfies it by
//! construction. This clause is SH-630's: on 2026-09-09 the guard below
//! permitted a worktree's `target/debug/story` to migrate the production store
//! 32 → 33, because its caller had put `target/debug` first on `$PATH` to try
//! it and the only question the guard asked was one `$PATH` answers.
//!
//! **Is this binary the `story` `$PATH` resolves?** SH-404's original clause,
//! kept as a second refusal: a copy at `/tmp/story` is outside any build
//! directory and still not what the operator runs. It can never again be the
//! *only* clause, because `$PATH` is a per-process claim the caller makes about
//! itself; `tests/migration_guard.rs`'s control used to be exactly the SH-630
//! invocation, and it passed.
//!
//! SH-404 considered and rejected "a build-provenance sentinel, in the
//! [`crate::env::is_test_build`] mould" — a feature flag set by `make install`
//! and the release workflow. That objection was to a *flag someone sets at
//! install time*, which every `cargo test` binary would lack too, so the guard
//! would have fired across the whole suite and needed an exemption nothing
//! could prove end to end. The build-directory stamp is a *location fact
//! recorded at the build*: a `cargo test` binary is uninstalled by construction
//! and is correctly refused, and the suite proves the permit side for real by
//! copying the binary out — which is all installing ever was. A per-migration
//! "released in" marker was rejected then and stays rejected: it would refuse
//! the recovery above.
//!
//! # The scope
//!
//! The guard applies only to the store a machine uses when nothing names one
//! ([`crate::env::StoreLocation::is_default`], reached here as
//! [`Inputs::is_default`]), not [`crate::env::StoreOrigin`]. Origin is the
//! wrong instrument: the daemon is always started with `--store-path` on its
//! own argv, so inside the one process that ever migrates an existing store,
//! origin is always `Flag`, never `XdgDefault`. An origin-keyed guard would
//! never fire. A store named explicitly with `--store-path` or
//! `$STORYHOOK_DATA_DIR` — a scratch store, a second tracker — is the caller's
//! deliberate choice and stays unguarded.
//!
//! This means `is_default()` is doing almost no work *inside this crate's own
//! test suite*: `storyhook_test_support::TestEnv` builds a fake `HOME` whose
//! store sits at the default-shaped path under it, so `is_default()` reads
//! `true` for nearly every fixture in the tree — the same gap
//! [`crate::env::is_test_build`]'s own refusal (`TEST_BUILD_REFUSAL`'s doc)
//! and [`crate::service::project::refuse_temp_project_in_real_store`] both
//! already record. It is still the right predicate: it is correct in
//! production, which is the only place `StoreOrigin` cannot be, and every
//! fixture that reaches [`crate::invoke::open_store`] opens a **fresh** store
//! (`from_version == 0`), which permits regardless. Every harness that runs
//! an uninstalled build with `target/debug` first on `$PATH` —
//! `plugins/story/tests/run-tests.sh`'s decoy-`story` fixtures,
//! `scripts/run-e2e.sh`, the `Makefile`'s plugin leg — is safe for that one
//! reason: the store there is always created fresh in the same run, so
//! nothing is ever pending. That is the one margin in `make test` this module
//! must not narrow: softening the fresh-store exemption would turn those legs
//! red. The two fixtures that *do* plant a non-zero schema and reach this
//! guard from a subprocess — `storyhook_test_support::crash` and `make
//! scratch` — say so, with [`OVERRIDE_VAR`] set on purpose.

use std::path::{Path, PathBuf};

use crate::path_identity;

/// The environment variable that deliberately bypasses this guard.
pub const OVERRIDE_VAR: &str = "STORYHOOK_ALLOW_UNINSTALLED_MIGRATION";

/// The facts the guard decides on, already resolved.
///
/// Every field is a plain value rather than something [`decide`] would have to
/// go compute — canonicalized paths, a version already read from the
/// database — because the decision itself does no I/O and needs none: a test
/// builds one of these by hand, the whole truth table becomes a table of
/// literals, and [`gather`] is the only place that ever touches the process
/// environment or the filesystem for real.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// The store this invocation is about to migrate.
    pub store_path: PathBuf,
    /// Whether `store_path` is the store a machine uses when nothing names
    /// one.
    pub is_default: bool,
    /// The schema version currently recorded in the store. `0` for a store
    /// with no schema at all.
    pub from_version: u32,
    /// The schema version this binary would migrate the store to.
    pub to_version: u32,
    /// This process's own executable, canonicalized. `None` only if the
    /// platform could not report it at all — [`std::env::current_exe`]
    /// failing is not a case this binary is known to hit on a supported
    /// target, and is treated the same as `$PATH` naming nothing: nothing is
    /// provably at risk, so the guard permits rather than inventing a second
    /// failure mode for an unreachable one.
    pub current_exe: Option<PathBuf>,
    /// The directory cargo wrote this binary into, canonicalized — `build.rs`'s
    /// stamp. `None` for a build that carried none (no `OUT_DIR`), in which
    /// case only the `$PATH` clause below can refuse.
    pub build_dir: Option<PathBuf>,
    /// The `story` `$PATH` resolves to, canonicalized. `None` if `$PATH` names
    /// none, or names one this process cannot read.
    pub installed_story: Option<PathBuf>,
    /// Whether [`OVERRIDE_VAR`] is set.
    pub override_set: bool,
}

/// Refuses to advance the default store's schema when this process is still
/// where cargo wrote it, or is not the `story` its own `$PATH` would run.
///
/// `Ok(())` permits the migration; `Err` carries the full refusal message.
/// The four leading conditions are a conjunction — any one being false
/// permits — and then either of two facts refuses, in this order: the build
/// directory (which the caller cannot rewrite) before `$PATH` (which it can).
pub fn decide(inputs: &Inputs) -> Result<(), String> {
    // A store named explicitly is the caller's deliberate choice. See the
    // module doc for why origin cannot stand in for this.
    if !inputs.is_default {
        return Ok(());
    }
    // A store with no schema yet has no peer binary depending on the schema it
    // has now — there is nothing for a migration to break. `migrate::run`
    // makes this same distinction for the same reason: it skips the
    // pre-migration backup entirely when `from_version == 0`.
    if inputs.from_version == 0 {
        return Ok(());
    }
    // Nothing pending, nothing to refuse.
    if inputs.to_version <= inputs.from_version {
        return Ok(());
    }
    if inputs.override_set {
        return Ok(());
    }
    // Fail open when this process cannot name itself: nothing provably
    // conflicts with what the operator runs. See the doc on `current_exe`; it
    // is believed unreachable on a supported target, not exercised.
    let Some(running) = &inputs.current_exe else {
        return Ok(());
    };
    // The fact the caller cannot rewrite, first (SH-630). A binary still in
    // its build directory is uninstalled whatever `$PATH` says, and `$PATH`
    // saying otherwise is exactly how the incident happened.
    if let Some(build_dir) = &inputs.build_dir
        && path_identity::is_inside_build_dir(running, build_dir)
    {
        return Err(uninstalled_message(inputs, running, build_dir));
    }
    // Then `$PATH`, failing open when it names no `story` at all: a binary
    // that has left its build directory, with nothing on `$PATH` to disagree
    // with, is the launchd-started daemon's ordinary shape and nothing there
    // is provably at risk.
    let Some(installed) = &inputs.installed_story else {
        return Ok(());
    };
    if running == installed {
        return Ok(());
    }
    Err(disagreement_message(inputs, running, installed))
}

/// The refusal for a binary that never left its build directory (SH-630):
/// names the store, both versions, the binary, the directory, and every way
/// out.
///
/// Like [`disagreement_message`], never `story update` (SH-405).
fn uninstalled_message(inputs: &Inputs, running: &Path, build_dir: &Path) -> String {
    format!(
        "refusing to migrate `{store}` from schema {from} to {to}: this binary ({running}) is \
         still where cargo built it ({build_dir}) — it has not been installed, whatever $PATH \
         says. Applying the migration would advance the store's schema past what the `story` \
         you actually use understands — the same refusal you would get reading a store a newer \
         storyhook wrote, met here from the other side. Nothing has been migrated.\n\n\
         If you meant to install this build, do that first (`make install`, or however this \
         tree normally installs) and run the installed `story` — a copy outside its build \
         directory is what installing means here. If you only meant to run this build once \
         against your real store, set ${override_var}=1 and run the command again; be aware \
         the store will then need a `story` at least this new to open at all.",
        store = inputs.store_path.display(),
        from = inputs.from_version,
        to = inputs.to_version,
        running = running.display(),
        build_dir = build_dir.display(),
        override_var = OVERRIDE_VAR,
    )
}

/// The refusal for a binary that is not the one `$PATH` runs (SH-404): names
/// the store, both versions, both executables, and every way out.
///
/// Deliberately does not point at `story update`: an unreleased migration has
/// no release to update *to*, and telling the operator to run a command that
/// reports up to date and changes nothing is the sibling defect this story's
/// own comment trail records (SH-405) — repeating that dead end here would
/// ship it twice.
fn disagreement_message(inputs: &Inputs, running: &Path, installed: &Path) -> String {
    format!(
        "refusing to migrate `{store}` from schema {from} to {to}: this binary ({running}) is \
         not the `story` your $PATH runs ({installed}). Applying the migration would advance \
         the store's schema past what the `story` you actually use understands — the same \
         refusal you would get reading a store a newer storyhook wrote, met here from the \
         other side. Nothing has been migrated.\n\n\
         If you meant to install this build, do that first (`make install`, or however this \
         tree normally installs), then run the command again — the two will agree and the \
         migration will proceed. If you only meant to run this build once against your real \
         store, set ${override_var}=1 and run the command again; be aware the store will then \
         need a `story` at least this new to open at all.",
        store = inputs.store_path.display(),
        from = inputs.from_version,
        to = inputs.to_version,
        running = running.display(),
        installed = installed.display(),
        override_var = OVERRIDE_VAR,
    )
}

/// Resolves the real inputs [`decide`] needs, for the one production caller.
///
/// Reads the process environment and this process's own identity; everything
/// past this point is pure. The split mirrors
/// [`crate::service::project::refuse_temp_project_in_real_store`] /
/// `refuse_temp_project` for the same reason stated there — a test that wants
/// to exercise the decision cannot safely mutate `$PATH` for one test among
/// many sharing a process, but it can build an [`Inputs`] by hand.
#[must_use]
pub fn gather(store_path: &Path, is_default: bool, from_version: u32, to_version: u32) -> Inputs {
    Inputs {
        store_path: store_path.to_path_buf(),
        is_default,
        from_version,
        to_version,
        current_exe: path_identity::running_exe().map(|exe| exe.canonical),
        build_dir: path_identity::build_dir(),
        installed_story: path_identity::installed_story().map(|story| story.canonical),
        override_set: std::env::var_os(OVERRIDE_VAR).is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SH-404's shape: a worktree's debug binary, a store below what it
    /// understands, a different `story` on `$PATH`, and no build stamp — the
    /// table as it stood before SH-630, so every row it proved still holds.
    fn base_inputs() -> Inputs {
        Inputs {
            store_path: PathBuf::from("/home/dev/.local/share/storyhook/store.db"),
            is_default: true,
            from_version: 16,
            to_version: 17,
            current_exe: Some(PathBuf::from("/home/dev/repo/target/debug/story")),
            build_dir: None,
            installed_story: Some(PathBuf::from("/home/dev/.local/bin/story")),
            override_set: false,
        }
    }

    /// SH-630's shape: the same binary, and this time `$PATH` *agrees* with
    /// it — because the caller put `target/debug` first — while the stamp
    /// says it never left its build directory.
    fn stamped_inputs() -> Inputs {
        let mut inputs = base_inputs();
        inputs.build_dir = Some(PathBuf::from("/home/dev/repo/target/debug"));
        inputs.installed_story = Some(PathBuf::from("/home/dev/repo/target/debug/story"));
        inputs
    }

    /// The SH-404 incident, stated as a test: refused, and the message names
    /// both executables.
    #[test]
    fn refuses_the_incident_shape() {
        let error = decide(&base_inputs()).expect_err("must refuse");
        assert!(error.contains("/home/dev/.local/share/storyhook/store.db"));
        assert!(error.contains("16"));
        assert!(error.contains("17"));
        assert!(error.contains("/home/dev/repo/target/debug/story"));
        assert!(error.contains("/home/dev/.local/bin/story"));
        assert!(error.contains(OVERRIDE_VAR));
        assert!(
            !error.contains("story update"),
            "must not repeat SH-405's dead end: {error}"
        );
    }

    /// The SH-630 incident, stated as a test: `$PATH` agreeing with the
    /// binary is no longer a permit when the binary is still where cargo
    /// wrote it. The message names the build directory and the install
    /// step, and — like every refusal here — never `story update`.
    #[test]
    fn refuses_a_binary_still_in_its_build_directory_even_when_path_agrees() {
        let error = decide(&stamped_inputs()).expect_err("must refuse");
        assert!(error.contains("/home/dev/.local/share/storyhook/store.db"));
        assert!(error.contains("16"));
        assert!(error.contains("17"));
        assert!(error.contains("/home/dev/repo/target/debug/story"));
        assert!(error.contains("/home/dev/repo/target/debug"));
        assert!(error.contains("make install"));
        assert!(error.contains(OVERRIDE_VAR));
        assert!(
            !error.contains("story update"),
            "must not repeat SH-405's dead end: {error}"
        );
    }

    /// The build directory is checked ahead of `$PATH`, so the launchd shape —
    /// nothing on `$PATH` at all — no longer fails open for an uninstalled
    /// binary. Compare `permits_when_path_names_no_story`, which is the same
    /// row without a stamp.
    #[test]
    fn refuses_a_binary_in_its_build_directory_even_when_path_names_no_story() {
        let mut inputs = stamped_inputs();
        inputs.installed_story = None;
        assert!(decide(&inputs).is_err());
    }

    /// Installing is copying out: the same binary, outside the stamped
    /// directory, with `$PATH` pointing at the copy — permitted.
    #[test]
    fn permits_a_copy_outside_the_build_directory() {
        let mut inputs = stamped_inputs();
        inputs.current_exe = Some(PathBuf::from("/home/dev/.local/bin/story"));
        inputs.installed_story = Some(PathBuf::from("/home/dev/.local/bin/story"));
        assert_eq!(decide(&inputs), Ok(()));
    }

    /// A copy outside the build directory is still refused by the SH-404
    /// clause when it is not what `$PATH` runs — the second fact is kept, not
    /// replaced.
    #[test]
    fn a_copy_outside_the_build_directory_still_answers_to_path() {
        let mut inputs = stamped_inputs();
        inputs.current_exe = Some(PathBuf::from("/tmp/story"));
        inputs.installed_story = Some(PathBuf::from("/home/dev/.local/bin/story"));
        let error = decide(&inputs).expect_err("must refuse");
        assert!(error.contains("/tmp/story"));
        assert!(error.contains("/home/dev/.local/bin/story"));
    }

    /// The override clears both clauses at once — it means "this build, this
    /// store, on purpose", whichever fact would otherwise refuse.
    #[test]
    fn the_override_clears_the_build_directory_clause_too() {
        let mut inputs = stamped_inputs();
        inputs.override_set = true;
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn permits_when_the_running_binary_is_the_installed_one() {
        let mut inputs = base_inputs();
        inputs.current_exe = Some(PathBuf::from("/home/dev/.local/bin/story"));
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn permits_a_store_named_explicitly() {
        let mut inputs = base_inputs();
        inputs.is_default = false;
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn permits_a_fresh_store() {
        let mut inputs = base_inputs();
        inputs.from_version = 0;
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn permits_when_nothing_is_pending() {
        let mut inputs = base_inputs();
        inputs.to_version = inputs.from_version;
        assert_eq!(decide(&inputs), Ok(()));
    }

    #[test]
    fn permits_when_the_override_is_set() {
        let mut inputs = base_inputs();
        inputs.override_set = true;
        assert_eq!(decide(&inputs), Ok(()));
    }

    /// The fail-open, stated so it stays measured rather than assumed: nothing
    /// on `$PATH` to disagree with is nothing provably at risk.
    #[test]
    fn permits_when_path_names_no_story() {
        let mut inputs = base_inputs();
        inputs.installed_story = None;
        assert_eq!(decide(&inputs), Ok(()));
    }

    /// The mirror case: this process cannot name itself. Believed unreachable
    /// on a supported target; still a fail-open rather than a second, untested
    /// failure mode.
    #[test]
    fn permits_when_this_process_cannot_name_itself() {
        let mut inputs = base_inputs();
        inputs.current_exe = None;
        assert_eq!(decide(&inputs), Ok(()));
    }
}
