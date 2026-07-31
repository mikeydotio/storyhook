//! A throwaway project may only be created in a throwaway store (SH-95).
//!
//! `test_build_guard.rs` closed this for *test builds*, keyed on the
//! `fault-injection` feature — exact, and it works. But it only covers binaries
//! built by `cargo test`. The binary on `$PATH`, the one `make install`
//! produces, is what every *other* tool's test suite invokes, and it still
//! resolved the real data home and created projects in it without complaint.
//!
//! One run of one neighbouring repository's suite put 394 projects into this
//! machine's real store, 234 of them carrying stories, because ~119 fixture
//! sites call `mktemp -d` and then `story init` without isolating anything.
//! Before the flip that was harmless: `init` wrote a `.storyhook/` directory
//! inside the fixture and it died with the fixture. One global store turned
//! every one of those sites into a permanent write, silently.
//!
//! So the guard here is about the *mismatch*, not about temporary paths. A
//! temporary project in a temporary store is what a correct test builds, and
//! this project's own suite does it ~1977 times a run.

use std::path::{Path, PathBuf};

use storyhook::store::{ReadOps, SqliteStore, Store};
use storyhook_test_support::{scratch_dir, story_binary};

/// A data home that is **not** under any temporary directory.
///
/// `CARGO_TARGET_TMPDIR` lives under `target/`, inside the checkout — which is
/// the point. Every other scratch path this harness can reach is deliberately
/// temporary, and a test for "the store is a real one" cannot be written with
/// one of those.
fn non_temporary_data_home(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating a non-temporary data home");
    dir
}

/// Runs `story init` in `cwd` against `data_home`, with nothing inherited.
///
/// `env_clear` matters for the same reason it does in `test_build_guard.rs`:
/// the variable under test must not be able to arrive from the ambient shell or
/// from `make test`.
fn init_in(cwd: &Path, data_home: &Path, allow: bool) -> std::process::Output {
    let mut cmd = std::process::Command::new(story_binary());
    cmd.current_dir(cwd)
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        // Local, never the daemon: this asks what *one* process resolves, and a
        // client that spawned a daemon would be asking a second process whose
        // environment this test did not build.
        .env("STORYHOOK_INVOKER", "local")
        .env("STORYHOOK_DATA_DIR", data_home)
        .args(["project", "init"]);
    if allow {
        cmd.env("STORYHOOK_ALLOW_TEMP_PROJECT", "1");
    }
    cmd.output().expect("running the binary under test")
}

/// [`init_in`], for `story project init` and its optional `PATH`.
fn init_project_in(
    cwd: &Path,
    path: Option<&str>,
    data_home: &Path,
    allow: bool,
) -> std::process::Output {
    let mut cmd = std::process::Command::new(story_binary());
    cmd.current_dir(cwd)
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("STORYHOOK_INVOKER", "local")
        .env("STORYHOOK_DATA_DIR", data_home)
        .args(["project", "init"]);
    if let Some(path) = path {
        cmd.arg(path);
    }
    if allow {
        cmd.env("STORYHOOK_ALLOW_TEMP_PROJECT", "1");
    }
    cmd.output().expect("running the binary under test")
}

/// Whether the store at `data_home` knows a project at `path`.
///
/// Asked through the public read path rather than by SQL, so it asserts what a
/// caller would actually see. If `store.db` was never created the answer is
/// plainly no, which is the strongest form of "nothing was written".
fn store_knows_project_at(data_home: &Path, path: &Path) -> bool {
    let db = data_home.join("store.db");
    if !db.exists() {
        return false;
    }
    let store = SqliteStore::open(db).expect("opening the store");
    store
        .read(|tx| Ok(tx.project_by_path(path)?.is_some()))
        .expect("reading the store")
}

/// The reported case, end to end: a fixture directory, a real store.
#[test]
fn init_in_a_temp_dir_is_refused_when_the_store_is_not_temporary() {
    let fixture = scratch_dir();
    let data_home = non_temporary_data_home("sh95-real-store");

    let out = init_in(fixture.path(), &data_home, false);

    assert_eq!(
        out.status.code(),
        Some(2),
        "expected a usage refusal; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("STORYHOOK_DATA_DIR"),
        "the message must name the variable that fixes the caller for good: {stderr}"
    );
    assert!(
        stderr.contains("STORYHOOK_ALLOW_TEMP_PROJECT"),
        "the message must name the override, so the refusal is never a dead end: {stderr}"
    );

    // The point of the whole exercise: nothing reached the store.
    assert!(
        !store_knows_project_at(&data_home, fixture.path()),
        "the refusal must happen before anything is written"
    );
    assert!(
        !fixture.path().join(".storyhook.toml").exists(),
        "no pointer file may be left behind by a refused init"
    );
}

/// The positive control, and the reason the guard tests two things rather than
/// one. If this ever fails, the fix has broken every correctly isolated suite
/// on the machine — including this project's own.
#[test]
fn init_in_a_temp_dir_is_allowed_when_the_store_is_temporary_too() {
    let fixture = scratch_dir();
    let data_home = scratch_dir();

    let out = init_in(fixture.path(), data_home.path(), false);

    assert_eq!(
        out.status.code(),
        Some(0),
        "a throwaway project in a throwaway store is what a correct test builds; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fixture.path().join(".storyhook.toml").exists(),
        "the pointer file should have been written"
    );
}

/// `story project init <PATH>` gave the guard a second thing to be wrong
/// about: it used to judge the working directory, and now the working
/// directory and the directory being created can differ.
///
/// These two cases are the halves of that, and they fail in opposite
/// directions. Judging the cwd would let the first one through — a fixture
/// creating a project in the real store from a safe cwd, which is SH-95 again
/// by another door — and would refuse the second, breaking a developer who
/// happens to be standing in `/tmp`.
mod an_explicit_path {
    use super::{init_project_in, non_temporary_data_home, store_knows_project_at};
    use storyhook_test_support::scratch_dir;

    #[test]
    fn is_what_the_guard_judges_rather_than_the_working_directory() {
        let safe_cwd = non_temporary_data_home("sh95-path-cwd");
        let fixture = scratch_dir();
        let data_home = non_temporary_data_home("sh95-path-real-store");

        let out = init_project_in(
            &safe_cwd,
            Some(fixture.path().to_str().unwrap()),
            &data_home,
            false,
        );

        assert_eq!(
            out.status.code(),
            Some(2),
            "a temporary *target* is the creation, wherever it was typed; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!store_knows_project_at(&data_home, fixture.path()));
        assert!(!fixture.path().join(".storyhook.toml").exists());
    }

    #[test]
    fn permits_a_real_target_named_from_a_temporary_working_directory() {
        let temp_cwd = scratch_dir();
        let target = non_temporary_data_home("sh95-path-real-target");
        let data_home = non_temporary_data_home("sh95-path-real-store-2");

        let out = init_project_in(
            temp_cwd.path(),
            Some(target.to_str().unwrap()),
            &data_home,
            false,
        );

        assert_eq!(
            out.status.code(),
            Some(0),
            "the cwd is not the creation; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(target.join(".storyhook.toml").exists());
    }
}

/// The refusal is never a wall with no way past.
#[test]
fn the_override_permits_a_temp_project_in_a_real_store() {
    let fixture = scratch_dir();
    let data_home = non_temporary_data_home("sh95-override");

    let out = init_in(fixture.path(), &data_home, true);

    assert_eq!(
        out.status.code(),
        Some(0),
        "an explicit override must be honoured; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
