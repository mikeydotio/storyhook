//! Fixture directories, deliberately sited outside the Spotlight-indexed
//! `$TMPDIR`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

/// Every scratch directory this harness hands out is named with this prefix,
/// which is also what makes the stale-fixture sweep safe: it only ever removes
/// entries it can prove the harness created.
const PREFIX: &str = "storyhook-fixture-";

/// Fixtures older than this are assumed to be debris from a run that died
/// without unwinding (a `SIGKILL`, an aborted process, a `TestEnv::shared()`
/// whose `TempDir` was never dropped because statics are not dropped). No
/// legitimate run lives anywhere near this long, so a survivor past it belongs
/// to nobody.
const STALE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// The base temp directory fixtures are created under.
///
/// Deliberately *not* `$TMPDIR`. On macOS `$TMPDIR` (`/var/folders/…/T/`) is
/// Spotlight-indexed, and this suite creates hundreds of tiny files per run
/// (temp git repos, storyhook project trees, registries); `mds_stores`
/// backlogs behind them and starves the fixture-heavy web tests until they
/// fail as unexplained 404s — a diagnosis that points nowhere near the
/// filesystem (SH-53). `/private/tmp` (the real path behind `/tmp`) is never
/// indexed. On every other platform the OS temp dir carries no such hazard.
fn unindexed_base() -> PathBuf {
    let unindexed = Path::new("/private/tmp");
    if cfg!(target_os = "macos") && unindexed.is_dir() {
        unindexed.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

/// The root every fixture directory in this harness is created under.
///
/// A dedicated directory rather than the temp base itself, so the stale-fixture
/// sweep has somewhere it can search exhaustively without ever looking at a
/// path this harness did not create.
///
/// # It proves nothing above it claims a project (SH-121)
///
/// Once per test binary, and it is what makes AC-2 true of the *whole* harness
/// rather than of the fixtures somebody remembered to check. Project resolution
/// climbs from the working directory, so a pointer file anywhere above this
/// root would be inherited by every fixture with none of its own — silently,
/// and every one of them would go on passing against a tracker nobody named.
/// The check costs three `stat` calls and turns that into a refusal to run.
pub fn scratch_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = unindexed_base().join("storyhook-tests");
        std::fs::create_dir_all(&root).unwrap_or_else(|e| {
            panic!("creating the fixture root {}: {e}", root.display());
        });
        ensure_tmpdir_is_spotlight_exempt();
        sweep_stale_fixtures(&root);
        crate::project::assert_selection_is_not_inherited(&root);
        root
    })
    .clone()
}

/// Best-effort: mark `$TMPDIR` itself Spotlight-exempt, for the sake of the
/// suites that still build fixtures there. The marker is untracked machine
/// state that an OS upgrade wipes (SH-53), so the harness creates it rather
/// than assuming a human did — a failure to create it is advisory, and
/// `scratch_dirs_live_outside_the_spotlight_indexed_tmpdir` is what makes it
/// loud.
fn ensure_tmpdir_is_spotlight_exempt() {
    if cfg!(target_os = "macos") {
        let marker = std::env::temp_dir().join(".metadata_never_index");
        if !marker.exists() {
            let _ = std::fs::File::create(&marker);
        }
    }
}

/// Removes fixtures left behind by runs that are long over.
///
/// Necessary because [`crate::TestEnv::shared`] parks its `TempDir` in a
/// `OnceLock`, and Rust never drops statics: a per-test-binary environment is
/// worth having, but it costs one undeleted directory per binary per run
/// unless something reclaims them. Best-effort throughout — a fixture the
/// current user cannot remove is somebody else's problem, not a reason to fail
/// a test run.
fn sweep_stale_fixtures(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Creates a fixture directory under [`scratch_root`]. Every temp directory
/// this harness creates goes through here — never `tempfile::tempdir()`, which
/// lands in the indexed `$TMPDIR` (see `clippy.toml`).
pub fn scratch_dir() -> TempDir {
    scratch_dir_named("")
}

/// [`scratch_dir`], with `label` folded into the directory name so a fixture
/// that outlives its run can be traced back to what made it.
pub fn scratch_dir_named(label: &str) -> TempDir {
    let root = scratch_root();
    tempfile::Builder::new()
        .prefix(&format!("{PREFIX}{label}"))
        .tempdir_in(&root)
        .unwrap_or_else(|e| panic!("creating a scratch directory in {}: {e}", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_dirs_live_outside_the_spotlight_indexed_tmpdir() {
        let dir = scratch_dir();
        let indexed = std::env::temp_dir();
        assert!(
            !dir.path().starts_with(&indexed),
            "fixtures must not be created under {} — macOS Spotlight indexes it, and this \
             suite's file churn backlogs mds_stores until the web tests fail as unexplained \
             404s (SH-53); got {}",
            indexed.display(),
            dir.path().display()
        );

        if cfg!(target_os = "macos") {
            let marker = indexed.join(".metadata_never_index");
            assert!(
                marker.exists(),
                "the harness must create {} so the suites that still use $TMPDIR are \
                 Spotlight-exempt too (SH-53)",
                marker.display()
            );
        }
    }

    #[test]
    fn scratch_dirs_are_distinct_and_removed_on_drop() {
        let first = scratch_dir();
        let second = scratch_dir();
        assert_ne!(first.path(), second.path());

        let path = first.path().to_path_buf();
        assert!(path.is_dir());
        drop(first);
        assert!(
            !path.exists(),
            "a fixture must be reclaimed when its guard drops, or every run leaks one"
        );
    }

    #[test]
    fn labelled_fixtures_carry_their_label_in_the_path() {
        let dir = scratch_dir_named("registry-");
        let name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            name.starts_with(&format!("{PREFIX}registry-")),
            "a labelled fixture must be traceable to its maker, got {name}"
        );
    }

    #[test]
    fn the_sweep_only_touches_this_harnesss_own_fixtures() {
        let root = scratch_root();
        // A foreign, ancient directory: right place, wrong name. Left
        // untouched because only the prefix proves ownership.
        //
        // Named per process for the same reason `ours` is: `scratch_root()` is
        // a machine-global path shared by every checkout, and this program runs
        // `make test` from a worktree while the main checkout may be running it
        // too. With a constant name the two runs collide on one directory, and
        // whichever finishes first removes it out from under the other's
        // assertion — a red gate describing a fixture, not a defect.
        let foreign = root.join(format!(
            "not-a-storyhook-fixture-do-not-delete-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&foreign).unwrap();
        filetime_set_epoch(&foreign);

        let ours = root.join(format!("{PREFIX}ancient-{}", std::process::id()));
        std::fs::create_dir_all(&ours).unwrap();
        filetime_set_epoch(&ours);

        sweep_stale_fixtures(&root);

        assert!(
            foreign.is_dir(),
            "the sweep must never remove a directory it cannot prove it created"
        );
        assert!(
            !ours.exists(),
            "the sweep must reclaim this harness's own long-dead fixtures"
        );
        let _ = std::fs::remove_dir_all(&foreign);
    }

    /// Backdates `path` well past [`STALE_AFTER`]. Uses `touch -t`, which is
    /// POSIX, rather than adding a crate dependency for four test lines.
    fn filetime_set_epoch(path: &Path) {
        let status = std::process::Command::new("touch")
            .args(["-t", "200001010000"])
            .arg(path)
            .status()
            .expect("running touch");
        assert!(status.success(), "backdating {}", path.display());
    }
}
