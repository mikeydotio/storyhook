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

use std::os::unix::process::ExitStatusExt;
use std::path::Path;

use storyhook::daemon::backup;
use storyhook::store::{ReadOps, SqliteStore, Store, StoryQuery};
use storyhook_test_support::{Project, TestEnv, project_id_at};

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

/// The titles of every story the CLI can see in `project`.
fn titles_via_cli(project: &Project<'_>) -> Vec<String> {
    project.json(&["list"])["stories"]
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

    let store = cli_shaped_store(&env);
    let snapshot = backup::run_if_due(&store, &env.environment())
        .expect("taking a snapshot")
        .expect("an empty backup directory is overdue");
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
    project.run(&["new", "after the restore"]).success();
    assert_eq!(titles_via_cli(&project).len(), before.len() + 1);
    project.run(&["doctor"]).success();
}

/// The trap: restoring over a database whose write-ahead log is still hot.
///
/// This is what a person actually does. Something crashed — so `store.db-wal`
/// still holds commits that were never checkpointed into `store.db` — and they
/// copy the newest snapshot over `store.db` because that is what the diagnostic
/// told them to do. The old database's log is now sitting beside the new
/// database's pages, and SQLite will try to replay one into the other.
///
/// The log has to be made hot the only way it can be: by killing a process that
/// had just committed. A `story` that exits normally checkpoints on the way out,
/// which is why this scenario needs a crash to set up and why it is a *recovery*
/// hazard rather than an everyday one.
#[test]
fn restoring_over_a_stale_write_ahead_log_does_not_silently_mix_two_databases() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("BK").build();
    project.new_story("in the snapshot");

    let store = cli_shaped_store(&env);
    let snapshot = backup::run_if_due(&store, &env.environment())
        .expect("taking a snapshot")
        .expect("a snapshot");
    drop(store);

    // A commit that survives its process: durable, in the log, uncheckpointed.
    let killed = env
        .raw_story(project.path())
        .env("STORYHOOK_INVOKER", "local")
        .env("STORYHOOK_FAULT", "after_commit_before_ack")
        .args(["new", "after the snapshot"])
        .output()
        .expect("running the doomed command");
    assert_eq!(
        killed.status.signal(),
        Some(libc::SIGKILL),
        "the fixture needs a process that died holding a hot log"
    );
    let wal = env.store_path().with_extension("db-wal");
    assert!(
        std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0),
        "and a non-empty write-ahead log to be about anything"
    );

    // The naive restore: the database only.
    std::fs::copy(&snapshot, env.store_path()).expect("restoring over the live database");

    let out = project.story().args(["list", "--json"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Both streams: an error travels in the JSON envelope on stdout under
    // `--json` and on stderr otherwise, and this test is about the words rather
    // than about which pipe carries them.
    let said = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        assert!(
            !stdout.contains("after the snapshot"),
            "a restored store must not have replayed the old database's log into the \
             new one's pages — that is two databases mixed together, reported as \
             success: {stdout}"
        );
    } else {
        assert!(
            said.contains("is damaged"),
            "if the restore cannot be made sense of, it must say so in storyhook's \
             words: {said}"
        );
        assert!(
            said.contains("-wal") && said.contains("-shm"),
            "and it must name the sidecar files, because leaving them behind is \
             exactly how the restore produced this: {said}"
        );
    }

    // The documented procedure — sidecars removed too — always works.
    remove_store(&env);
    std::fs::copy(&snapshot, env.store_path()).expect("restoring properly");
    let titles = titles_via_cli(&project);
    assert_eq!(titles, vec!["in the snapshot".to_string()]);
}

/// The corruption diagnostic points at the backups; this checks the two ends
/// agree, so a user following the message arrives at files that are there.
#[test]
fn the_corruption_diagnostic_names_the_directory_the_snapshots_are_in() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("BK").build();
    project.new_story("real data");

    let store = cli_shaped_store(&env);
    let snapshot = backup::run_if_due(&store, &env.environment())
        .expect("taking a snapshot")
        .expect("a snapshot");
    drop(store);
    remove_store(&env);
    std::fs::write(env.store_path(), b"not a database any more").expect("breaking the store");

    let out = project.story().arg("list").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
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

    for round in 0..12 {
        project.new_story(&format!("story {round}"));
        // `run_if_due` would decline every one of these: the previous snapshot
        // is seconds old, and the schedule is not what this test is about.
        store.snapshot(&dir).expect("taking a snapshot");
    }
    assert_eq!(
        backup::snapshots(&dir).len(),
        12,
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
        "`story daemon status` must mention the backups even when there are \
         none: {before}"
    );

    let store = cli_shaped_store(&env);
    backup::run_if_due(&store, &env.environment()).expect("taking a snapshot");
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
        .snapshot(&backups_dir(&env))
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
