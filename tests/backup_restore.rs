//! The backups, exercised as a recovery procedure rather than as a feature.
//!
//! `src/daemon/backup.rs` has unit tests for the schedule and the pruning. What
//! it cannot test from inside is the thing the backups exist for: that a person
//! whose tracker is damaged can get it back by following the instructions
//! storyhook prints, using the `story` binary they have.
//!
//! So this file runs the drill. It takes real snapshots of a real store,
//! destroys the store, restores it the way the diagnostic says to, and then
//! asks the CLI whether the data is there.
//!
//! # The trap the drill exists to catch
//!
//! The store is in write-ahead-logging mode, so `store.db` is not the whole
//! database: `store.db-wal` holds commits that have not been checkpointed into
//! it yet. Copying a snapshot over `store.db` alone therefore leaves the *old*
//! database's log beside the *new* database's pages, and SQLite will try to
//! replay one into the other. A restore procedure that says only "copy the
//! backup over the database" is a procedure that can make things worse, which
//! is why the drill performs it exactly as written rather than as intended.

use std::path::Path;

use storyhook::daemon::backup;
use storyhook::store::{FaultPoint, ReadOps, SqliteStore, Store, StoryQuery};
use storyhook_test_support::{Project, TestEnv, crash_the_daemon, project_id_at};

/// Where a CLI-driven store puts its snapshots: the state home, not beside the
/// data.
///
/// `data_home` is what a user might point at a synced directory, and a backup of
/// a database is exactly the thing that must not be synced back over the
/// database. The corruption diagnostic names this directory, so the drill has to
/// use the same one.
fn backups_dir(env: &TestEnv) -> std::path::PathBuf {
    env.state_home().join("storyhook/backups")
}

/// A store handle configured the way the CLI configures it.
fn cli_shaped_store(env: &TestEnv) -> SqliteStore {
    let mut config = storyhook::store::StoreConfig::new(env.store_path());
    config.backup_dir = backups_dir(env);
    let store = SqliteStore::open_with(config).expect("opening the store");
    store.migrate().expect("migrating");
    store
}

/// Runs `story <args>` in the project and returns the raw output.
///
/// The wrapper exists to hand back the `Output` rather than an assertion handle.
fn story_in(project: &Project<'_>, args: &[&str]) -> std::process::Output {
    project.story().args(args).output().expect("running story")
}

/// Stands the daemon down, immediately before the store is handled as a file.
///
/// Every fixture here used to be built with `--local`, so no daemon ever existed
/// to confuse the questions this file asks — and every question it asks is about
/// bytes on disk. There is one transport now, so a daemon exists whether or not
/// a test wants one: it answers reads from its own page cache, and its `-shm`
/// keeps a write-ahead log alive that would otherwise be checkpointed away.
///
/// **The call belongs immediately before the bytes are touched, not once at the
/// top of a test.** Every `story` command starts a daemon if none is live, so a
/// fixture that quiesced itself at construction is holding the store again by
/// its second line.
///
/// Which makes this the honest version of what `--local` was standing in for,
/// rather than a workaround: it is exactly what storyhook's own restore
/// instructions tell a user to do first, and
/// `the_corruption_diagnostic_names_the_directory_the_snapshots_are_in` asserts
/// they still say so.
fn quiesce(env: &TestEnv) {
    env.stop_daemon();
}

/// Takes a snapshot now, whatever the schedule thinks.
///
/// `run_if_due` is the wrong tool for a fixture: it declines when a recent
/// snapshot exists, and under `make test-daemon` a daemon has already taken one
/// at startup — which is how the daemon leg caught four tests in this file
/// assuming an empty backup directory. The schedule has its own tests; these
/// are about the files.
fn snapshot_now(store: &SqliteStore, env: &TestEnv) -> std::path::PathBuf {
    store
        .snapshot(&backups_dir(env), "snapshot")
        .expect("taking a snapshot")
}

/// The titles of every story the CLI can see in `project`.
fn titles_via_cli(project: &Project<'_>) -> Vec<String> {
    let out = story_in(project, &["list", "--json"]);
    assert!(
        out.status.success(),
        "`story list --json` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("`story list --json` prints JSON");
    json["stories"]
        .as_array()
        .expect("`story list --json` returns an array")
        .iter()
        .map(|entry| entry["story"]["title"].as_str().unwrap_or("").to_string())
        .collect()
}

/// Removes a store and everything SQLite keeps beside it.
fn remove_store(env: &TestEnv) {
    for suffix in ["", "-wal", "-shm"] {
        let path = env.store_path().with_file_name(format!("store.db{suffix}"));
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// The drill
// ---------------------------------------------------------------------------

/// The whole procedure, end to end: back up, lose everything, restore, work.
///
/// The store is destroyed rather than merely corrupted, because a restore that
/// only works when the original is still partly there is not a restore.
#[test]
fn a_snapshot_restores_a_store_that_was_destroyed() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("BK").build();
    project.new_story("before the backup");
    let before = titles_via_cli(&project);

    quiesce(&env);
    let store = cli_shaped_store(&env);
    let snapshot = snapshot_now(&store, &env);
    drop(store);

    remove_store(&env);
    assert!(!env.store_path().exists(), "the store is gone");

    // The documented restore: copy the snapshot into place. Nothing else is
    // needed, because `remove_store` took the sidecars with it — which is what
    // the diagnostic has to tell people to do, and what the next test checks it
    // does.
    std::fs::copy(&snapshot, env.store_path()).expect("restoring the snapshot");

    assert_eq!(
        titles_via_cli(&project),
        before,
        "the restored store must hold what the snapshot held"
    );
    // And it is a live store, not a museum piece.
    assert!(
        story_in(&project, &["new", "after the restore"])
            .status
            .success()
    );
    assert_eq!(titles_via_cli(&project).len(), before.len() + 1);
    assert!(story_in(&project, &["doctor"]).status.success());
}

/// The trap: restoring over a database whose write-ahead log is still hot.
///
/// This is what a person actually does. Something crashed — so `store.db-wal`
/// still holds commits that were never checkpointed into `store.db` — and they
/// copy the newest snapshot over `store.db`, because "copy the backup over the
/// database" is what a restore is. The old log is now beside the new pages, and
/// SQLite does what it is supposed to do with a log: replays it.
///
/// **The naive restore is therefore not a restore**, and the outcome is not
/// even deterministic — with a daemon holding the store it silently produces a
/// mixture of two databases reported as success, and without one it produces
/// `database disk image is malformed`. Neither is the snapshot. That is why the
/// instructions name three files and a `story daemon stop`, and this test exists
/// to keep them naming them: it asserts the hazard is real and that the
/// documented procedure clears it.
///
/// The hot log has to be made the only way it can be: by killing a process that
/// had just committed. A `story` that exits normally checkpoints on the way out,
/// which is why this is a *recovery* hazard rather than an everyday one.
#[test]
fn the_documented_restore_is_necessary_as_well_as_sufficient() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("BK").build();
    project.new_story("in the snapshot");

    quiesce(&env);
    let store = cli_shaped_store(&env);
    let snapshot = snapshot_now(&store, &env);
    drop(store);

    // A commit that survives its process: durable, in the log, uncheckpointed.
    // The process has to be the daemon, because the daemon is the one holding
    // the transaction — and it has to be killed rather than stopped, because a
    // process that exits normally checkpoints the log this test is about.
    crash_the_daemon(
        &env,
        project.path(),
        FaultPoint::AfterCommitBeforeAck,
        &["new", "after the snapshot"],
    );
    let wal = env.store_path().with_extension("db-wal");
    assert!(
        std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0),
        "and a non-empty write-ahead log to be about anything"
    );

    // Necessary: the naive restore does not produce the snapshot.
    std::fs::copy(&snapshot, env.store_path()).expect("restoring over the live database");
    let out = story_in(&project, &["list", "--json"]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The property is that the naive restore **fails**, not that it fails with
    // one of two particular sentences. It has produced a third since SH-130
    // changed the shape of `stories`: the stale write-ahead log replays pages
    // laid out for the old table over the new one, and the read comes back with
    // a column type that cannot be decoded. Which flavour of wrong you get
    // depends on the schema, so asserting on the flavour made this test a
    // tripwire for unrelated migrations. The exit status is the durable signal.
    assert!(
        !out.status.success(),
        "the naive restore is supposed to be unsafe — the old log is replayed into \
         the new pages, or the result is malformed. If it has become safe, this test \
         and the restore instructions both want revisiting: {said}"
    );
    assert_ne!(
        titles_after_a_successful_list(&out),
        Some(vec!["in the snapshot".to_string()]),
        "and it must not quietly look like it worked"
    );

    // Sufficient: the documented procedure produces exactly the snapshot. Step
    // one of it is standing the daemon down, which the naive attempt above just
    // started — and which is the step that makes the rest work.
    quiesce(&env);
    remove_store(&env);
    std::fs::copy(&snapshot, env.store_path()).expect("restoring properly");
    assert_eq!(
        titles_via_cli(&project),
        vec!["in the snapshot".to_string()]
    );
    assert!(story_in(&project, &["doctor"]).status.success());
}

/// The titles in a `story list --json` output, or `None` if it failed.
fn titles_after_a_successful_list(out: &std::process::Output) -> Option<Vec<String>> {
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(
        json["stories"]
            .as_array()?
            .iter()
            .map(|entry| entry["story"]["title"].as_str().unwrap_or("").to_string())
            .collect(),
    )
}

/// The corruption diagnostic points at the backups; this checks the two ends
/// agree, so a user following the message arrives at files that are there.
#[test]
fn the_corruption_diagnostic_names_the_directory_the_snapshots_are_in() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("BK").build();
    project.new_story("real data");

    quiesce(&env);
    let store = cli_shaped_store(&env);
    let snapshot = snapshot_now(&store, &env);
    drop(store);
    remove_store(&env);
    std::fs::write(env.store_path(), b"not a database any more").expect("breaking the store");

    let out = story_in(&project, &["list"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a store that is no longer a database must fail: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let named = backups_dir(&env).display().to_string();
    assert!(
        stderr.contains(&named),
        "the diagnostic must name the directory the snapshots are actually in \
         ({named}): {stderr}"
    );
    assert!(
        snapshot.starts_with(backups_dir(&env)),
        "and the snapshots must actually be there: {}",
        snapshot.display()
    );
    assert!(
        stderr.contains("story daemon stop"),
        "and it must say to stop the daemon first — a daemon holding the old \
         database serves it happily while the files are swapped underneath: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Rotation and verification
// ---------------------------------------------------------------------------

/// Rotation, exercised with real snapshots of a real store rather than with
/// files named like one.
///
/// `backup.rs`'s own test writes twelve text files to check the arithmetic. This
/// takes twelve `VACUUM INTO` copies of a database that is changing between
/// them, and then asserts not only that seven survive but that all seven open —
/// a rotation that keeps seven corrupt files is worse than one that keeps none,
/// because it looks fine.
#[test]
fn rotation_keeps_seven_snapshots_and_every_one_of_them_is_a_database() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("BK").build();
    let store = cli_shaped_store(&env);
    let dir = backups_dir(&env);

    // Relative to whatever is already there: under `make test-daemon` a daemon
    // has taken one at startup, and a fixture that assumes an empty directory
    // is a fixture that only passes on one of the two legs.
    let already = backup::snapshots(&dir).len();
    for round in 0..12 {
        project.new_story(&format!("story {round}"));
        // `run_if_due` would decline every one of these: the previous snapshot
        // is seconds old, and the schedule is not what this test is about.
        store.snapshot(&dir, "snapshot").expect("taking a snapshot");
    }
    assert_eq!(
        backup::snapshots(&dir).len(),
        already + 12,
        "the fixture must have overfilled the directory before rotation runs"
    );

    // Rotation happens when a snapshot is *taken*, so the directory is pruned
    // by the daemon's next due run rather than continuously. Backdating the
    // newest is what makes that run due, and is the only way to reach the real
    // entry point without waiting a day.
    backdate(backup::snapshots(&dir).last().expect("a newest"), 2);
    backup::run_if_due(&store, &env.environment())
        .expect("the daily run")
        .expect("a backdated snapshot makes one due");
    drop(store);

    let kept = backup::snapshots(&dir);
    assert!(
        kept.len() <= backup::RETAIN,
        "rotation must bound the count at {}, found {}",
        backup::RETAIN,
        kept.len()
    );
    assert_eq!(kept.len(), backup::RETAIN, "and it must keep that many");

    for path in &kept {
        assert_openable(path);
    }
    // Ordered by name, and the name carries the timestamp — so the survivors are
    // the newest, which is the half of "keep seven" that matters.
    let mut sorted = kept.clone();
    sorted.sort();
    assert_eq!(kept, sorted, "snapshots must be reported oldest-first");
}

/// Moves a file's modification time `days` into the past.
///
/// The backup schedule is a question about wall-clock age, and the alternative
/// to rewriting the timestamp is waiting a day.
fn backdate(path: &Path, days: u64) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("opening {} to backdate it: {e}", path.display()));
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 24 * 60 * 60);
    file.set_modified(when)
        .unwrap_or_else(|e| panic!("backdating {}: {e}", path.display()));
}

/// The pre-migration backup, checked for the property it is taken for: it holds
/// the database as it was *before* the schema moved.
#[test]
fn the_pre_migration_backup_holds_the_old_schema_and_opens() {
    let env = TestEnv::isolated();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema/v1.db");
    std::fs::create_dir_all(env.data_dir()).expect("creating the data directory");
    std::fs::copy(&fixture, env.store_path()).expect("planting a v1 store");

    let store = cli_shaped_store(&env);
    drop(store);

    let kept = backup::snapshots(&backups_dir(&env));
    assert_eq!(kept.len(), 1, "one migration, one backup: {kept:?}");
    let backup_path = &kept[0];
    assert!(
        backup_path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains("-v1")),
        "the name must say which version it was taken from: {}",
        backup_path.display()
    );

    let conn = rusqlite::Connection::open(backup_path).expect("opening the backup");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("reading the backup's schema version");
    assert_eq!(
        version, 1,
        "the backup must hold the schema the database had before the migration — that \
         is the entire point of taking it first"
    );
    assert_openable(backup_path);
}

/// Every snapshot must be a database SQLite is happy with. `VACUUM INTO` plus a
/// reopen and an `integrity_check` is what `snapshot` promises; this is the
/// promise checked from outside.
fn assert_openable(path: &Path) {
    let conn = rusqlite::Connection::open(path)
        .unwrap_or_else(|e| panic!("{} must reopen: {e}", path.display()));
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap_or_else(|e| panic!("{} must be checkable: {e}", path.display()));
    assert_eq!(verdict, "ok", "{} must be sound", path.display());
}

// ---------------------------------------------------------------------------
// Surfacing
// ---------------------------------------------------------------------------

/// A backup nobody has looked at is a belief rather than a backup, so storyhook
/// reports their age. It is `story daemon status` that says it and not `story
/// doctor` — doctor's bytes are pinned by the golden corpus and its exit code
/// means a project's integrity, while a backup's age is a fact about the
/// machine.
#[test]
fn the_backups_age_is_reachable_from_the_command_line() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("BK").build();

    let before = String::from_utf8_lossy(
        &project
            .story()
            .args(["daemon", "status"])
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(
        before.contains("backups"),
        "`story daemon status` must mention the backups, whether or not there \
         are any yet: {before}"
    );

    let store = cli_shaped_store(&env);
    snapshot_now(&store, &env);
    drop(store);

    let after = String::from_utf8_lossy(
        &project
            .story()
            .args(["daemon", "status"])
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(
        after.contains("newest"),
        "and once there is one, how old it is: {after}"
    );
}

/// The store the snapshot was taken from is untouched by taking it — obvious,
/// and worth a test because `VACUUM INTO` is the one backup mechanism that runs
/// *through* the database engine rather than around it.
#[test]
fn taking_a_snapshot_does_not_disturb_the_store() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("BK").build();
    project.new_story("one");
    project.new_story("two");
    let before = titles_via_cli(&project);

    let store = cli_shaped_store(&env);
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    let counted = store
        .read(|tx| Ok(tx.stories(pid, &StoryQuery::all())?.len()))
        .expect("counting");
    store
        .snapshot(&backups_dir(&env), "snapshot")
        .expect("taking a snapshot");
    assert_eq!(
        store
            .read(|tx| Ok(tx.stories(pid, &StoryQuery::all())?.len()))
            .expect("counting again"),
        counted
    );
    drop(store);

    assert_eq!(titles_via_cli(&project), before);
}
