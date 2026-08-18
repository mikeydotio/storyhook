//! A binary that is not the `story` on `$PATH` must not carry the machine's
//! default store forward (SH-404).
//!
//! `cargo build`, in a worktree or anywhere else, produces a binary that
//! resolves the real data home and applies every pending migration to it —
//! one-way, with no prompt and no notice. On 2026-08-17 a worktree's debug
//! binary carrying the then-unreleased migration 17 moved this machine's real
//! store from `PRAGMA user_version` 16 to 17. The installed release binary
//! understood only 16, so every `story` command failed at daemon start with
//! the SH-54 forward-compatibility gate's own message, exit 5. Migration 17
//! was in no release, so recovery required building from `main` and
//! installing by hand.
//!
//! SH-54 gave the *read* side a gate: an older binary refuses a newer store
//! with one clear sentence. This is its write-side counterpart
//! (`storyhook::migration_guard`): a binary that is not the one `$PATH` would
//! run refuses to advance the *default* store's schema at all.
//!
//! | case | expected |
//! |---|---|
//! | planted v1 store, a decoy `story` first on `$PATH` | refused; exit 2; names the store, both versions, both executables, the override |
//! | …and after that refusal | `PRAGMA user_version` and `schema_migrations` are unchanged — nothing was written |
//! | planted v1 store, the normal `$PATH` | succeeds; migrates to the current schema — the control |
//! | planted v1 store, decoy `$PATH`, the override set | succeeds |
//! | a fresh store, decoy `$PATH` | succeeds — a store with no schema has no peer binary to break |
//! | planted v1 store, nothing named `story` on `$PATH` | succeeds — the fail-open, measured rather than claimed |

use std::ffi::OsString;
use std::path::Path;

use rusqlite::Connection;
use storyhook::migration_guard::OVERRIDE_VAR;
use storyhook_test_support::{TestEnv, scratch_dir, story_binary};

/// A store at schema v1, planted where `story` will find it.
///
/// The same fixture and construction as `crash_matrix.rs`'s
/// `env_with_a_v1_store`: the committed v1 database is the only *old* one in
/// the tree, and it exists so a migration has something real to migrate.
fn env_with_a_v1_store() -> TestEnv {
    let env = TestEnv::isolated();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema/v1.db");
    std::fs::create_dir_all(env.data_dir()).expect("creating the data directory");
    std::fs::copy(&fixture, env.store_path()).expect("planting the v1 fixture");
    env
}

/// `PRAGMA user_version` of the database at `path`.
fn user_version(path: &Path) -> u32 {
    Connection::open(path)
        .expect("opening the database")
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map(|v| u32::try_from(v).expect("a non-negative schema version"))
        .expect("reading user_version")
}

/// How many rows `schema_migrations` holds — the framework's own record of
/// what it applied, checked separately from the pragma so a refusal that
/// somehow wrote the stamp without recording itself (or the reverse) would
/// still be caught.
fn migrations_applied(path: &Path) -> i64 {
    Connection::open(path)
        .expect("opening the database")
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("counting schema_migrations")
}

/// A directory holding one executable file named `story` — never run; it only
/// has to be what `$PATH` *resolves*, which is the entire comparison the guard
/// makes. A real, executable file rather than an empty placeholder, because a
/// shell — and this guard's own `resolve_on_path` — skips a `PATH` entry that
/// is not an executable file, silently.
fn decoy_story_dir() -> (tempfile::TempDir, OsString) {
    let dir = scratch_dir();
    let decoy = dir.path().join("story");
    std::fs::write(&decoy, b"#!/bin/sh\nexit 1\n").expect("writing the decoy");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&decoy)
            .expect("reading the decoy's metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&decoy, perms).expect("marking the decoy executable");
    }
    let path = dir.path().as_os_str().to_owned();
    (dir, path)
}

/// A `$PATH` with nothing named `story` on it at all — one empty directory,
/// so the answer does not depend on whether the machine running this suite
/// happens to have `story` installed for real.
fn path_without_story() -> (tempfile::TempDir, OsString) {
    let dir = scratch_dir();
    let path = dir.path().as_os_str().to_owned();
    (dir, path)
}

// ---------------------------------------------------------------------------
// The refusal
// ---------------------------------------------------------------------------

/// The incident, as a test: a binary that is not the one `$PATH` names must
/// not migrate the default store.
#[test]
fn a_binary_that_is_not_the_one_on_path_refuses_to_migrate_the_default_store() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    let (_decoy, hostile_path) = decoy_story_dir();
    assert_eq!(
        user_version(env.store_path()),
        1,
        "the fixture must start at v1"
    );

    let out = env
        .raw_story(cwd.path())
        .env("PATH", &hostile_path)
        .args(["project", "list"])
        .output()
        .expect("running story");

    assert!(
        !out.status.success(),
        "expected a refusal, got success:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "a refusal to migrate is a usage failure (distinct from the SH-54 read \
         gate's exit 5), not an integrity one"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        stderr.contains(&env.store_path().display().to_string()),
        "the store: {stderr}"
    );
    assert!(
        stderr.contains("from schema 1 to"),
        "both versions: {stderr}"
    );
    assert!(
        stderr.contains(&story_binary().display().to_string())
            || stderr.contains("target/debug/story")
            || stderr.contains("target/debug/deps"),
        "the binary actually running: {stderr}"
    );
    assert!(
        stderr.contains(OVERRIDE_VAR),
        "the way through, if this was deliberate: {stderr}"
    );
    assert!(
        !stderr.contains("story update"),
        "must not repeat SH-405's dead end — `story update` cannot fix a store \
         an unreleased build advanced: {stderr}"
    );

    // The bytes. Stood down first, immediately before the store is touched as
    // a file, because a live daemon answers reads from its own page cache —
    // the standing rule this project's own CLAUDE.md states for exactly this
    // shape of assertion.
    env.stop_daemon();
    assert_eq!(
        user_version(env.store_path()),
        1,
        "a refused migration must leave the schema version it found"
    );
    assert_eq!(
        migrations_applied(env.store_path()),
        1,
        "and must not have recorded a migration it did not run"
    );
}

// ---------------------------------------------------------------------------
// The four ways this must NOT fire
// ---------------------------------------------------------------------------

/// The control: the same store, the same command, the ordinary `$PATH` a
/// developer actually has — and it migrates normally. Without this, the test
/// above would pass equally well for a guard that refuses everything.
#[test]
fn the_binary_that_path_names_migrates_normally() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    assert_eq!(
        user_version(env.store_path()),
        1,
        "the fixture must start at v1"
    );

    let status = env
        .raw_story(cwd.path())
        .args(["project", "list"])
        .status()
        .expect("running story");
    assert!(status.success(), "an ordinary run must succeed: {status}");

    env.stop_daemon();
    assert_eq!(
        user_version(env.store_path()),
        storyhook::store::current_schema_version(),
        "an ordinary run must migrate all the way forward"
    );
}

/// The deliberate override — named in the refusal message, so it can be found
/// without reading source, the same idiom `STORYHOOK_ALLOW_TEMP_PROJECT` uses
/// elsewhere in this tree.
#[test]
fn the_override_lets_an_uninstalled_binary_migrate_on_purpose() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    let (_decoy, hostile_path) = decoy_story_dir();

    let status = env
        .raw_story(cwd.path())
        .env("PATH", &hostile_path)
        .env(OVERRIDE_VAR, "1")
        .args(["project", "list"])
        .status()
        .expect("running story");
    assert!(
        status.success(),
        "the override must let the migration through: {status}"
    );

    env.stop_daemon();
    assert_eq!(
        user_version(env.store_path()),
        storyhook::store::current_schema_version(),
        "the override must let the migration proceed all the way forward"
    );
}

/// A fresh store has no peer binary to break — the condition the guard checks
/// before it ever looks at `$PATH`. This is what keeps the guard from
/// breaking a first run out of a worktree, and it is the exact shape that
/// keeps `make test`'s own plugin harness green: it also shadows `story` on
/// `$PATH` while creating its store fresh in the same run.
#[test]
fn a_fresh_store_is_created_and_migrated_under_a_hostile_path() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();
    let (_decoy, hostile_path) = decoy_story_dir();

    let status = env
        .raw_story(cwd.path())
        .env("PATH", &hostile_path)
        .args(["project", "list"])
        .status()
        .expect("running story");
    assert!(
        status.success(),
        "a fresh store must not be refused: {status}"
    );

    env.stop_daemon();
    assert_eq!(
        user_version(env.store_path()),
        storyhook::store::current_schema_version(),
        "a fresh store migrates to the current schema like any other first run"
    );
}

/// The fail-open, measured rather than claimed: with nothing named `story` on
/// `$PATH`, there is nothing to disagree with, so the migration proceeds. A
/// launchd-started daemon takes exactly this branch — the plist
/// `daemon::commands` writes carries no `PATH` at all.
#[test]
fn no_story_on_path_permits_the_migration() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    let (_empty, empty_path) = path_without_story();
    assert_eq!(
        user_version(env.store_path()),
        1,
        "the fixture must start at v1"
    );

    let status = env
        .raw_story(cwd.path())
        .env("PATH", &empty_path)
        .args(["project", "list"])
        .status()
        .expect("running story");
    assert!(
        status.success(),
        "nothing on $PATH means nothing provably at risk: {status}"
    );

    env.stop_daemon();
    assert_eq!(
        user_version(env.store_path()),
        storyhook::store::current_schema_version(),
        "the fail-open lets the migration proceed all the way forward"
    );
}
