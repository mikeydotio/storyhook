//! The migration framework, its backup discipline, and the
//! forward-compatibility gate.
//!
//! Everything here is file-backed. A `:memory:` database cannot be reopened,
//! has no write-ahead log, and cannot be backed up — which are the three things
//! this module is about.

use std::path::Path;

use rusqlite::Connection;
use storyhook::store::migrate::{self, Migration};
use storyhook::store::{SqliteStore, Store, StoreConfig, StoreError};
use storyhook_test_support::scratch_dir;

/// A second migration, used to exercise the parts of the framework that only
/// exist once a database has something in it worth backing up. There is only
/// one real migration in the tree, and there will not be a second until a real
/// schema change needs one.
fn with_second_migration(sql: &'static str) -> Vec<Migration> {
    let mut list = migrate::MIGRATIONS.to_vec();
    list.push(Migration {
        version: 2,
        name: "test-only",
        sql,
    });
    list
}

fn user_version(path: &Path) -> u32 {
    let conn = Connection::open(path).unwrap();
    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .unwrap()
        .try_into()
        .unwrap()
}

#[test]
fn a_fresh_database_migrates_to_the_current_version() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();

    let report = store.migrate().unwrap();

    assert_eq!(report.from_version, 0);
    assert_eq!(report.to_version, migrate::current_schema_version());
    assert_eq!(report.applied, vec!["initial".to_string()]);
    assert!(!report.is_noop());
    assert_eq!(user_version(store.path()), 1);
}

#[test]
fn an_empty_database_is_not_backed_up_because_it_has_nothing_to_lose() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();

    let report = store.migrate().unwrap();

    assert_eq!(report.backup, None);
    assert!(
        !store.backup_dir().exists(),
        "an empty database should not have produced a backup directory"
    );
}

#[test]
fn migrating_twice_changes_nothing() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let report = store.migrate().unwrap();

    assert!(report.is_noop());
    assert_eq!(report.from_version, 1);
    assert_eq!(report.to_version, 1);
    assert_eq!(report.backup, None);
}

#[test]
fn the_migration_history_records_every_step() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let conn = Connection::open(store.path()).unwrap();
    let rows: Vec<(u32, String)> = conn
        .prepare("SELECT version, name FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();

    assert_eq!(rows, vec![(1, "initial".to_string())]);
    let applied_at: String = conn
        .query_row("SELECT applied_at FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        applied_at.ends_with('Z') && applied_at.contains('T'),
        "applied_at should be RFC3339 UTC, got `{applied_at}`"
    );
}

#[test]
fn a_migrated_database_reopens_at_its_recorded_version() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    SqliteStore::open(&path).unwrap().migrate().unwrap();

    // A fresh process would do exactly this: open, find nothing pending.
    let reopened = SqliteStore::open(&path).unwrap();
    assert!(reopened.migrate().unwrap().is_noop());
}

#[test]
fn write_ahead_logging_survives_a_reopen() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    SqliteStore::open(&path).unwrap().migrate().unwrap();

    let conn = Connection::open(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

// ---------------------------------------------------------------------------
// The forward-compatibility gate (SH-54)
// ---------------------------------------------------------------------------

#[test]
fn a_database_from_a_newer_storyhook_is_refused_with_one_clear_message() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    SqliteStore::open(&path).unwrap().migrate().unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 99")
        .unwrap();

    let error = SqliteStore::open(&path).unwrap_err();

    match error {
        StoreError::SchemaTooNew { found, supported } => {
            assert_eq!(found, 99);
            assert_eq!(supported, migrate::current_schema_version());
        }
        other => panic!("expected SchemaTooNew, got: {other}"),
    }
    let message = SqliteStore::open(&path).unwrap_err().to_string();
    assert!(message.contains("newer storyhook"), "{message}");
    assert!(message.contains("story update"), "{message}");
}

#[test]
fn migrating_a_database_from_a_newer_storyhook_is_also_refused() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    let store = SqliteStore::open(&path).unwrap();
    store.migrate().unwrap();
    // Bumped behind the store's back, as a newer binary sharing the file would.
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 5")
        .unwrap();

    let error = store.migrate().unwrap_err();

    assert!(
        matches!(error, StoreError::SchemaTooNew { found: 5, .. }),
        "expected SchemaTooNew, got: {error}"
    );
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

#[test]
fn a_pending_migration_takes_a_verified_backup_first() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let report = store
        .migrate_with(&with_second_migration("CREATE TABLE later (x INTEGER);"))
        .unwrap();

    let backup = report.backup.expect("a v1 database must be backed up");
    assert!(backup.exists(), "{} was not written", backup.display());
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("-v1"),
        "the backup should name the version it was taken from: {}",
        backup.display()
    );
    assert_eq!(report.from_version, 1);
    assert_eq!(report.to_version, 2);
}

#[test]
fn the_backup_is_a_real_restorable_database_not_a_hot_file_copy() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();
    // Leave data in the write-ahead log, unflushed. `fs::copy` of this file
    // would produce a backup missing the row.
    Connection::open(store.path())
        .unwrap()
        .execute_batch(
            "INSERT INTO projects (uuid, slug, name, prefix, created_at) \
             VALUES ('u', 's', 'n', 'SH', '2026-01-01T00:00:00Z')",
        )
        .unwrap();

    let report = store
        .migrate_with(&with_second_migration("CREATE TABLE later (x INTEGER);"))
        .unwrap();

    let backup = Connection::open(report.backup.unwrap()).unwrap();
    let verdict: String = backup
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(verdict, "ok");
    let slug: String = backup
        .query_row("SELECT slug FROM projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        slug, "s",
        "the backup must contain data that was still in the write-ahead log"
    );
    let version: i64 = backup
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1, "the backup is of the pre-migration schema");
}

#[test]
fn a_backup_that_cannot_be_written_stops_the_migration() {
    let dir = scratch_dir();
    let store = SqliteStore::open_with(StoreConfig {
        // A *file* where the backup directory should be: `create_dir_all` on it
        // fails, which is the cheapest honest way to make the backup step fail.
        backup_dir: dir.path().join("occupied"),
        ..StoreConfig::new(dir.path().join("store.db"))
    })
    .unwrap();
    store.migrate().unwrap();
    std::fs::write(dir.path().join("occupied"), b"not a directory").unwrap();

    let error = store
        .migrate_with(&with_second_migration("CREATE TABLE later (x INTEGER);"))
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Backup(_)),
        "expected a Backup error, got: {error}"
    );
    assert!(
        error.to_string().contains("no migration was attempted"),
        "the message must say the database was left alone: {error}"
    );
    assert_eq!(
        user_version(store.path()),
        1,
        "the database must be untouched when its backup failed"
    );
}

#[test]
fn a_backup_that_cannot_be_verified_stops_the_migration() {
    use storyhook::store::FaultPoint;
    use storyhook::store::fault::{FaultAction, arm};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let _fault = arm(
        FaultPoint::BackupVerify,
        FaultAction::Fail("integrity check says no".to_string()),
    );
    let error = store
        .migrate_with(&with_second_migration("CREATE TABLE later (x INTEGER);"))
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Backup(_)),
        "expected a Backup error, got: {error}"
    );
    assert_eq!(
        user_version(store.path()),
        1,
        "an unverifiable backup must leave the database at its old version"
    );
}

// ---------------------------------------------------------------------------
// Failure isolation
// ---------------------------------------------------------------------------

#[test]
fn a_failing_migration_leaves_the_database_at_its_previous_version() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let error = store
        .migrate_with(&with_second_migration("THIS IS NOT SQL;"))
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Migration { version: 2, .. }),
        "expected a Migration error naming version 2, got: {error}"
    );
    assert_eq!(user_version(store.path()), 1);
    // And the store is still usable: the failed transaction rolled back rather
    // than leaving a half-applied schema behind.
    assert!(store.migrate().unwrap().is_noop());
}

#[test]
fn a_migration_interrupted_between_steps_keeps_what_it_finished() {
    use storyhook::store::FaultPoint;
    use storyhook::store::fault::{FaultAction, arm};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();

    // Fire on the *first* pending migration of a fresh database, before
    // anything is applied.
    let fault = arm(
        FaultPoint::MidMigration,
        FaultAction::Fail("interrupted".to_string()),
    );
    assert!(store.migrate().is_err());
    drop(fault);

    assert_eq!(
        user_version(store.path()),
        0,
        "nothing should have been applied"
    );
    assert_eq!(store.migrate().unwrap().to_version, 1);
}

#[test]
fn concurrent_migrations_of_one_store_apply_exactly_once() {
    let dir = scratch_dir();
    let store = std::sync::Arc::new(SqliteStore::open(dir.path().join("store.db")).unwrap());

    let applied: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = std::sync::Arc::clone(&store);
                scope.spawn(move || store.migrate().unwrap().applied.len())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });

    assert_eq!(
        applied, 1,
        "exactly one of eight concurrent callers should have applied the migration"
    );
    assert_eq!(user_version(store.path()), 1);
}
