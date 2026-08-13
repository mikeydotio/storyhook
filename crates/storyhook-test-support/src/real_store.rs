//! A fixture directory the store guards classify as **not throwaway**.
//!
//! `refuse_temp_project_in_real_store`, `refuse_project_burst_in_real_store` and
//! `story doctor`'s catalog audit all key off [`storyhook::service::project::
//! is_under_temp`], and four test sites need a base that predicate rejects so
//! those guards stay exercised rather than silently inert. Every one of them
//! used to express "not throwaway" as *"inside the checkout's `target/`
//! directory"* — `$CARGO_MANIFEST_DIR/target/<label>` or
//! `$CARGO_TARGET_TMPDIR/<label>`, four copies of the same assumption. That
//! assumption is only true while the checkout itself is not temporary, and it
//! is silently false from a checkout under `/private/tmp` — a fresh disposable
//! clone, which is the correct tool for cutting a release, bisecting, or
//! reproducing against pristine `origin/main`. From there both sides of every
//! one of those guards read as throwaway, each guard is correctly inert, and
//! the tests that expect a refusal fail with messages naming neither the store
//! nor the reason (SH-258).
//!
//! [`non_temporary_dir`] resolves its base independently of the checkout and
//! verifies the choice with the production predicate, so the premise cannot
//! quietly drift from the guard it exists to exercise.

use std::env;
use std::path::PathBuf;
use std::sync::OnceLock;

use storyhook::service::project::is_under_temp;

/// Names an explicit fixture root, tried before either fallback.
///
/// The same idiom as `STORYHOOK_ALLOW_TEMP_PROJECT` and
/// `STORYHOOK_ALLOW_PROJECT_BURST`: an escape hatch that is named in the
/// failure message rather than discovered by reading source.
pub const REAL_ROOT_VAR: &str = "STORYHOOK_TEST_REAL_ROOT";

/// A directory [`storyhook::service::project::is_under_temp`] rejects,
/// resolved independently of the checkout — unlike the four call sites this
/// replaced, which inferred it from `CARGO_MANIFEST_DIR` or
/// `CARGO_TARGET_TMPDIR` and were therefore wrong whenever the checkout itself
/// was temp-rooted (SH-258).
///
/// The base is resolved once per test binary and memoized, the same shape
/// [`crate::story_binary`] and [`crate::scratch_root`] use. `label` names a
/// subdirectory under that base, removed and recreated on every call — the
/// same reset-per-call contract the four original per-file helpers had, so a
/// leftover write from a previous run can never leak into the next one's
/// assertions. Because every call site names its label from a small fixed set
/// (`"sh95-real-store"` and its siblings), the directories this creates do not
/// grow unbounded the way a uniquely-named-per-run fixture would, and no
/// staleness sweep is needed the way [`crate::scratch_dir`]'s is.
///
/// # Panics
///
/// If every candidate base is temporary or unavailable. Never silently skips
/// the caller's test: a skip here would disable the SH-95 path guard, the
/// SH-122 burst guard, or doctor's catalog audit in exactly the situation a
/// disposable checkout is used for.
pub fn non_temporary_dir(label: &str) -> PathBuf {
    let base = real_store_root();
    let dir = base.join(label);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| {
        panic!(
            "creating a non-temporary fixture directory at {}: {e}",
            dir.display()
        )
    });
    dir
}

/// [`non_temporary_dir`]'s base, resolved once per process.
fn real_store_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| choose_base(&candidates()).unwrap_or_else(|message| panic!("{message}")))
        .clone()
}

/// The candidate bases, most to least preferred, each paired with the name
/// [`choose_base`]'s failure message identifies it by.
///
/// A function rather than a constant because two of the three read the
/// environment or the running binary's own path — inputs [`choose_base`]
/// deliberately does not touch, so it can be tested against literal paths.
fn candidates() -> Vec<(&'static str, Option<PathBuf>)> {
    vec![
        (REAL_ROOT_VAR, env::var_os(REAL_ROOT_VAR).map(PathBuf::from)),
        ("<cargo target dir>/real-fixtures", target_dir_candidate()),
        (
            "$HOME/.cache/storyhook/test-real-fixtures",
            home_candidate(),
        ),
    ]
}

/// A directory beside the running test binary — `<target>/<profile>/real-fixtures`.
///
/// Resolved from `current_exe()` rather than `env!("CARGO_TARGET_TMPDIR")`,
/// which is unset for a `--lib` unit test binary and for this very crate's own
/// tests. [`crate::story_binary`] resolves the sibling `story` binary the same
/// way, popping a trailing `deps` segment for the same reason: an integration
/// test binary and a `--lib` unit test binary both run from
/// `<target>/<profile>/deps/<binary>`.
fn target_dir_candidate() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    Some(dir.join("real-fixtures"))
}

/// A directory under `$HOME`, independent of the checkout entirely — the
/// fallback for the one case the other two cannot cover: a temp-rooted
/// checkout with `$STORYHOOK_TEST_REAL_ROOT` unset.
fn home_candidate() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/storyhook/test-real-fixtures"))
}

/// The decision behind [`real_store_root`], with candidate resolution lifted
/// out into `candidates` — the same split `refuse_temp_project`/
/// `refuse_temp_project_in_real_store` uses in `storyhook::service::project`,
/// so this can be tested against literal paths rather than the process
/// environment or a real `current_exe()`.
///
/// Tried in order; the first present candidate [`is_under_temp`] rejects wins.
/// If none qualifies, the error names every candidate tried and its reason —
/// unset, unavailable, or temporary — plus [`REAL_ROOT_VAR`], so a future
/// temp-rooted run reports why instead of failing downstream with a message
/// that names neither the store nor the cause.
fn choose_base(candidates: &[(&str, Option<PathBuf>)]) -> Result<PathBuf, String> {
    let mut tried = Vec::with_capacity(candidates.len());
    for (name, candidate) in candidates {
        let Some(path) = candidate else {
            tried.push(format!("  {name}: unavailable"));
            continue;
        };
        if is_under_temp(path) {
            tried.push(format!("  {name}: {} (temporary)", path.display()));
            continue;
        }
        return Ok(path.clone());
    }
    Err(format!(
        "this test needs a fixture directory the store guards will classify as real, and \
         every candidate was temporary or unavailable:\n{}\n\nSet {REAL_ROOT_VAR} to a \
         directory outside any temporary root, or run this suite from a checkout that is not \
         itself under one.",
        tried.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real(path: &str) -> Option<PathBuf> {
        Some(PathBuf::from(path))
    }

    fn temp(path: &str) -> Option<PathBuf> {
        Some(std::env::temp_dir().join(path))
    }

    #[test]
    fn the_first_present_non_temporary_candidate_wins() {
        let chosen = choose_base(&[
            ("a", temp("a")),
            ("b", real("/Volumes/Code/mikeyward/storyhook-fixtures/b")),
            ("c", real("/Volumes/Code/mikeyward/storyhook-fixtures/c")),
        ])
        .expect("a real candidate was present");
        assert_eq!(
            chosen,
            PathBuf::from("/Volumes/Code/mikeyward/storyhook-fixtures/b"),
            "the first non-temporary candidate must win over a later one, not just any real one"
        );
    }

    #[test]
    fn an_absent_candidate_is_skipped_rather_than_chosen() {
        let chosen = choose_base(&[
            ("a", None),
            ("b", real("/Volumes/Code/mikeyward/storyhook-fixtures/b")),
        ])
        .expect("the second candidate was present and real");
        assert_eq!(
            chosen,
            PathBuf::from("/Volumes/Code/mikeyward/storyhook-fixtures/b")
        );
    }

    #[test]
    fn every_candidate_temporary_or_absent_is_a_named_error_not_a_silent_skip() {
        let err = choose_base(&[("a", temp("a")), ("b", None), ("c", temp("c"))])
            .expect_err("no candidate qualified");
        assert!(
            err.contains('a') && err.contains('c'),
            "must name every candidate tried: {err}"
        );
        assert!(
            err.contains("unavailable"),
            "must say why 'b' was skipped: {err}"
        );
        assert!(
            err.contains(REAL_ROOT_VAR),
            "must name the lever that unblocks the failure: {err}"
        );
    }

    #[test]
    fn no_candidates_at_all_is_also_a_named_error() {
        assert!(choose_base(&[]).is_err());
    }

    /// The real resolution end to end, exercised against this build's actual
    /// `current_exe()` and environment rather than literal paths — the one
    /// test in this module [`choose_base`]'s separation exists to keep the
    /// rest from needing.
    #[test]
    fn non_temporary_dir_resolves_to_a_path_the_production_guard_rejects_as_temporary() {
        let dir = non_temporary_dir("sh258-self-test");
        assert!(
            !is_under_temp(&dir),
            "non_temporary_dir must hand back a path the store guards would treat as real, \
             got {}",
            dir.display()
        );
        assert!(dir.is_dir());
    }
}
