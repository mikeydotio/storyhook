//! A binary that is not installed must not carry the machine's default store
//! forward (SH-404, SH-630).
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
//! (`storyhook::migration_guard`). SH-404 asked one question — *is this the
//! `story` `$PATH` runs?* — and on 2026-09-09 the answer was yes for a
//! `target/debug/story` whose caller had put `target/debug` first on `$PATH`
//! to try it (SH-630): the production store went 32 → 33 and the installed
//! release served it read-only. `$PATH` is the caller's own claim about
//! itself. The guard now also asks a question the caller cannot answer for
//! it: *has this binary left the directory cargo wrote it into?* `build.rs`
//! stamps that directory at the build, and a binary still inside it is
//! refused whatever `$PATH` says.
//!
//! **The control here used to be the incident.** The old control ran the
//! test binary with its own directory first on `$PATH` — exactly the
//! SH-630 invocation — and asserted the migration proceeded. The control is
//! now an *installed copy*: the test binary copied out of its build
//! directory, which is what `make install`, `story update` and `cargo install`
//! all do, and the only thing any of them does.
//!
//! | case | expected |
//! |---|---|
//! | planted v1 store, the test binary with its own directory first on `$PATH` (SH-630) | refused; exit 2; names the build directory; nothing written |
//! | planted v1 store, the test binary, a decoy `story` first on `$PATH` (SH-404) | refused; exit 2; names both executables, the override; nothing written |
//! | planted v1 store, the installed copy, its own directory on `$PATH` | succeeds; migrates to the current schema — the control |
//! | planted v1 store, the test binary, decoy `$PATH`, the override set | succeeds |
//! | a fresh store, the test binary, decoy `$PATH` | succeeds — a store with no schema has no peer binary to break |
//! | planted v1 store, the installed copy, nothing named `story` on `$PATH` | succeeds — the fail-open, measured rather than claimed |
//! | planted v1 store, the test binary, nothing named `story` on `$PATH` | refused — the launchd shape SH-411 met, closed on this side too |
//!
//! Every case that runs the installed copy first checks that the copy is
//! *outside* the stamped build directory and the test binary is *inside* it
//! (`storyhook_test_support::installed_copy`, shared with the seat guard's
//! tests since SH-634). A build with no stamp would otherwise pass every row
//! above vacuously.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use storyhook::migration_guard::OVERRIDE_VAR;
use storyhook_test_support::{TestEnv, installed_copy, scratch_dir, story_binary};

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

/// The directory `build.rs` stamped into this test binary — the one every
/// row in this file's table turns on. A test binary without it cannot prove
/// anything here, so its absence is a panic rather than a skipped case.
fn stamped_build_dir() -> PathBuf {
    storyhook::path_identity::build_dir().expect(
        "this test binary carries no STORYHOOK_BUILD_DIR stamp — build.rs did not run with \
         OUT_DIR, so nothing in this file can tell an installed binary from an uninstalled one",
    )
}

/// Runs the installed copy the way `TestEnv::raw_story` runs the test binary —
/// same isolation, same working directory — with `$PATH` set to `path`.
fn installed_story(env: &TestEnv, cwd: &Path, path: &OsString) -> std::process::Command {
    let mut cmd = env.raw_installed_story(cwd);
    cmd.env("PATH", path);
    cmd
}

/// The assertions every refusal shares: exit 2, the store named, both
/// versions named, the override named, no `story update`, and — after the
/// daemon is stood down, immediately before the file is touched — the bytes
/// unchanged.
fn assert_refused(env: &TestEnv, out: &std::process::Output) -> String {
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
    stderr
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

/// SH-630, as a test: the binary's own directory first on `$PATH`, so the
/// SH-404 comparison agrees with itself — and the migration is refused anyway,
/// because the binary is still where cargo wrote it.
#[test]
fn a_binary_still_where_cargo_wrote_it_refuses_even_with_its_own_directory_first_on_path() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    let (_decoy, decoy_path) = decoy_story_dir();
    // Positive control for this row specifically: the binary IS inside the
    // stamped directory. `installed_copy` checks the same thing; this row
    // must not depend on a permit-side test having run first.
    let build_dir = stamped_build_dir();
    let running = std::fs::canonicalize(story_binary()).expect("canonicalizing the test binary");
    assert!(
        running.starts_with(&build_dir),
        "the binary under test must sit inside the stamped build directory"
    );
    let own_dir = story_binary()
        .parent()
        .expect("the test binary has a directory")
        .to_path_buf();
    let path = std::env::join_paths([own_dir.as_os_str(), decoy_path.as_os_str()])
        .expect("joining PATH entries");
    assert_eq!(
        user_version(env.store_path()),
        1,
        "the fixture must start at v1"
    );

    let out = env
        .raw_story(cwd.path())
        .env("PATH", &path)
        .args(["project", "list"])
        .output()
        .expect("running story");

    let stderr = assert_refused(&env, &out);
    assert!(
        stderr.contains(&build_dir.display().to_string()),
        "the refusal must name the build directory the binary never left: {stderr}"
    );
    assert!(
        stderr.contains("make install"),
        "the refusal must name the way to install it: {stderr}"
    );
}

/// SH-404, as a test: a binary that is not the one `$PATH` names must not
/// migrate the default store. Still refused, and still for its own reason —
/// the message names both executables.
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

    let stderr = assert_refused(&env, &out);
    assert!(
        stderr.contains(&story_binary().display().to_string())
            || stderr.contains("target/debug/story")
            || stderr.contains("target/debug/deps"),
        "the binary actually running: {stderr}"
    );
}

/// The launchd shape SH-411 met on the install side: a plist carries no
/// `PATH`, so a login daemon started from a worktree build has nothing to
/// disagree with. SH-404's fail-open let it migrate; a binary still in its
/// build directory is refused before `$PATH` is ever consulted.
#[test]
fn no_story_on_path_still_refuses_a_binary_in_its_build_directory() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    let (_empty, empty_path) = path_without_story();

    let out = env
        .raw_story(cwd.path())
        .env("PATH", &empty_path)
        .args(["project", "list"])
        .output()
        .expect("running story");

    let stderr = assert_refused(&env, &out);
    assert!(
        stderr.contains(&stamped_build_dir().display().to_string()),
        "the refusal must name the build directory: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// The four ways this must NOT fire
// ---------------------------------------------------------------------------

/// The control: the same store, the same command, the binary installed —
/// copied out of its build directory, its own directory on `$PATH` — and it
/// migrates normally. Without this, every refusal above would pass equally
/// well for a guard that refuses everything.
#[test]
fn the_installed_copy_migrates_normally() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    let own_dir = installed_copy()
        .parent()
        .expect("the installed copy has a directory")
        .as_os_str()
        .to_owned();
    assert_eq!(
        user_version(env.store_path()),
        1,
        "the fixture must start at v1"
    );

    let out = installed_story(&env, cwd.path(), &own_dir)
        .args(["project", "list"])
        .output()
        .expect("running the installed copy");
    assert!(
        out.status.success(),
        "an installed binary must migrate normally: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    env.stop_daemon();
    assert_eq!(
        user_version(env.store_path()),
        storyhook::store::current_schema_version(),
        "an ordinary run must migrate all the way forward"
    );
}

/// The deliberate override — named in the refusal message, so it can be found
/// without reading source, the same idiom `STORYHOOK_ALLOW_TEMP_PROJECT` uses
/// elsewhere in this tree. It clears both clauses: the binary is in its build
/// directory *and* not the one on `$PATH`.
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
/// before it ever looks at the binary or `$PATH`. This is what keeps the guard
/// from breaking a first run out of a worktree, and it is the exact shape that
/// keeps `make test`'s own plugin harness and `scripts/run-e2e.sh` green: both
/// run an uninstalled build, with `target/debug` first on `$PATH`, against a
/// store created fresh in the same run.
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

/// The fail-open, measured rather than claimed: an installed binary with
/// nothing named `story` on `$PATH` has nothing to disagree with, so the
/// migration proceeds. A launchd-started daemon takes exactly this branch —
/// the plist `daemon::commands` writes carries no `PATH` at all — and since
/// SH-411 that plist names an installed binary, never a build directory.
#[test]
fn no_story_on_path_permits_the_installed_copy() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    let (_empty, empty_path) = path_without_story();
    assert_eq!(
        user_version(env.store_path()),
        1,
        "the fixture must start at v1"
    );

    let out = installed_story(&env, cwd.path(), &empty_path)
        .args(["project", "list"])
        .output()
        .expect("running the installed copy");
    assert!(
        out.status.success(),
        "nothing on $PATH means nothing provably at risk: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    env.stop_daemon();
    assert_eq!(
        user_version(env.store_path()),
        storyhook::store::current_schema_version(),
        "the fail-open lets the migration proceed all the way forward"
    );
}
