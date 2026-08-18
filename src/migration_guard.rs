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
//! # The predicate
//!
//! The guard asks whether the running executable *is* the `story` that `$PATH`
//! resolves — the story's own wording ("the binary currently installed on
//! PATH") made checkable offline. Two alternatives were considered and
//! rejected:
//!
//! - A build-provenance sentinel, in the [`crate::env::is_test_build`] mould
//!   (a feature flag set by `make install` and the release workflow, read back
//!   with `cfg!`). Every `cargo test` binary would also lack it, so the guard
//!   would fire across the whole suite and need a test-build exemption of its
//!   own — making it impossible to prove end-to-end. It would also need
//!   `build.rs` machinery this crate does not have.
//! - A per-migration "released in" marker. It would also refuse the documented
//!   recovery from this very incident — `make install` from `main`, carrying
//!   the unreleased migration on purpose — turning the fix into a dead end.
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
//! (`from_version == 0`), which permits regardless. The one place in this
//! repository's own test tree where `is_default()` is true *and* a fixture
//! plants a non-zero schema *and* `$PATH` is deliberately shadowed —
//! `plugin/claude-code/tests/run-tests.sh`'s decoy-`story` fixtures — is safe
//! only because the store there is always created fresh in the same run, so
//! nothing is ever pending. That is the one margin in `make test` this module
//! must not narrow: softening the fresh-store exemption would turn that
//! plugin leg red.

use std::env;
use std::path::{Path, PathBuf};

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
    /// The `story` `$PATH` resolves to, canonicalized. `None` if `$PATH` names
    /// none, or names one this process cannot read.
    pub installed_story: Option<PathBuf>,
    /// Whether [`OVERRIDE_VAR`] is set.
    pub override_set: bool,
}

/// Refuses to advance the default store's schema when this process is not the
/// `story` its own `$PATH` would run.
///
/// `Ok(())` permits the migration; `Err` carries the full refusal message.
/// Every one of the five conditions below is necessary — this is not a
/// heuristic score, it is a conjunction, and any one being false permits.
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
    // Fail open when there is nothing to compare against: `$PATH` naming no
    // `story`, or this process being unable to name itself, both mean nothing
    // provably conflicts with what the operator runs. See the doc on
    // `current_exe` for the second case; it is believed unreachable on a
    // supported target, not exercised.
    let (Some(running), Some(installed)) = (&inputs.current_exe, &inputs.installed_story) else {
        return Ok(());
    };
    if running == installed {
        return Ok(());
    }
    Err(refusal_message(inputs, running, installed))
}

/// The full refusal, naming the store, both versions, both executables, and
/// every way out.
///
/// Deliberately does not point at `story update`: an unreleased migration has
/// no release to update *to*, and telling the operator to run a command that
/// reports up to date and changes nothing is the sibling defect this story's
/// own comment trail records (SH-405) — repeating that dead end here would
/// ship it twice.
fn refusal_message(inputs: &Inputs, running: &Path, installed: &Path) -> String {
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
        current_exe: std::env::current_exe()
            .ok()
            .map(|exe| canonicalize_or(exe.clone(), exe)),
        installed_story: resolve_on_path(env::var_os("PATH").as_deref(), "story"),
        override_set: env::var_os(OVERRIDE_VAR).is_some(),
    }
}

/// Resolves `name` against `path` the way a shell would — the first entry
/// holding an executable, regular file by that name — and canonicalizes it.
///
/// Nothing in this crate resolves a bare command name against `$PATH` today,
/// and no dependency in `Cargo.toml` offers it, so this is the ~15 lines that
/// would otherwise be a new dependency for one lookup. It checks the
/// executable bit it claims to check — unlike the `is_executable` SH-198 found
/// and deleted, whose doc promised exactly this and whose body never performed
/// it.
///
/// Takes `path` as a value rather than reading `$PATH` itself so a test can
/// drive it without touching the process environment — see [`gather`]'s doc.
/// An empty entry is skipped rather than read as `.`, the way a shell would:
/// resolving `story` out of whichever directory a command happened to run in
/// is not a question this guard should ask.
fn resolve_on_path(path: Option<&std::ffi::OsStr>, name: &str) -> Option<PathBuf> {
    let path = path?;
    for dir in env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(canonicalize_or(candidate.clone(), candidate));
        }
    }
    None
}

/// Whether `path` is a regular file with at least one executable bit set.
///
/// Unix-only permission bits, unconditionally: every target in the release
/// matrix (`.github/workflows/release.yml`) is a Unix target, so this needs no
/// `cfg(target_os = "windows")` arm naming a platform nothing here builds for
/// (the class SH-260/SH-276 removed).
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// `std::fs::canonicalize`, falling back to the literal path on failure — the
/// same fallback `update`'s own exe resolution uses (`src/update.rs`), for the
/// same reason: a path that cannot be canonicalized is still the best answer
/// available, and the caller comparing it is about to fail for a better
/// reason than this function inventing a third state.
fn canonicalize_or(path: PathBuf, fallback: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_inputs() -> Inputs {
        Inputs {
            store_path: PathBuf::from("/home/dev/.local/share/storyhook/store.db"),
            is_default: true,
            from_version: 16,
            to_version: 17,
            current_exe: Some(PathBuf::from("/home/dev/repo/target/debug/story")),
            installed_story: Some(PathBuf::from("/home/dev/.local/bin/story")),
            override_set: false,
        }
    }

    /// The incident this story fixes, stated as a test: a worktree's debug
    /// binary, a store already at a schema below what it understands, and a
    /// different `story` on `$PATH` — refused.
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

    // -- resolve_on_path -----------------------------------------------------

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("writing a fake `story`");
        let mut perms = std::fs::metadata(path)
            .expect("reading metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("setting the executable bit");
    }

    #[test]
    fn resolves_the_first_executable_entry() {
        let dir = storyhook_test_support::scratch_dir();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        make_executable(&second.join("story"));
        make_executable(&first.join("story"));

        let path = env::join_paths([&first, &second]).unwrap();
        let found = resolve_on_path(Some(path.as_os_str()), "story").expect("a match");
        assert_eq!(found, std::fs::canonicalize(first.join("story")).unwrap());
    }

    /// A directory entry that has nothing by that name is skipped, not fatal —
    /// the same behaviour a shell has walking `$PATH`.
    #[test]
    fn skips_an_entry_with_nothing_by_that_name() {
        let dir = storyhook_test_support::scratch_dir();
        let empty = dir.path().join("empty");
        let real = dir.path().join("real");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        make_executable(&real.join("story"));

        let path = env::join_paths([&empty, &real]).unwrap();
        let found = resolve_on_path(Some(path.as_os_str()), "story").expect("a match");
        assert_eq!(found, std::fs::canonicalize(real.join("story")).unwrap());
    }

    /// A non-executable file by the right name is not a match — the whole
    /// point of checking the bit rather than only the name (SH-198).
    #[test]
    fn a_non_executable_file_is_not_a_match() {
        let dir = storyhook_test_support::scratch_dir();
        std::fs::write(dir.path().join("story"), b"not a program").unwrap();

        let path = env::join_paths([dir.path()]).unwrap();
        assert_eq!(resolve_on_path(Some(path.as_os_str()), "story"), None);
    }

    /// An empty `$PATH` entry is POSIX for "the current directory" — and this
    /// guard must never resolve `story` against whatever directory a command
    /// happened to be run from. `env::join_paths` refuses to emit an empty
    /// component at all, so this constructs the raw OS string by hand; a
    /// bare empty string is one such entry (`std::env::split_paths` yields it
    /// as a single empty component), and it is deterministic regardless of
    /// this test process's own working directory precisely because the empty
    /// entry is skipped *before* anything is ever joined against it.
    #[test]
    fn an_empty_path_entry_is_skipped_rather_than_read_as_the_current_directory() {
        assert_eq!(
            resolve_on_path(Some(std::ffi::OsStr::new("")), "story"),
            None
        );
    }

    /// The same skip, with a real match on either side — proving the empty
    /// entry is bypassed rather than merely happening to find nothing, and
    /// that a genuine entry after it is still reached.
    #[test]
    fn an_empty_entry_between_two_real_ones_does_not_stop_the_search() {
        let dir = storyhook_test_support::scratch_dir();
        make_executable(&dir.path().join("story"));
        let mut raw = std::ffi::OsString::from(":");
        raw.push(dir.path());
        assert_eq!(
            resolve_on_path(Some(raw.as_os_str()), "story").as_deref(),
            Some(
                std::fs::canonicalize(dir.path().join("story"))
                    .unwrap()
                    .as_path()
            )
        );
    }

    #[test]
    fn no_path_at_all_resolves_nothing() {
        assert_eq!(resolve_on_path(None, "story"), None);
    }
}
