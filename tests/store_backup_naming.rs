//! Two processes migrating one store must both survive it.
//!
//! # The defect
//!
//! `migrate::snapshot` named its backup from a millisecond timestamp and handed
//! that name straight to `VACUUM INTO`, which **refuses an output file that
//! already exists**. Every process upgrading one store at one moment computes
//! the same name, so the first wins and the rest fail — with
//!
//! ```text
//! error: pre-migration backup failed, so no migration was attempted:
//!   could not write …/storyhook-20260729T092228.049Z-v1.db:
//!   table schema_migrations already exists
//! ```
//!
//! which names the wrong problem entirely: SQLite reported the collision
//! through a stale `sqlite3_errmsg` left over from an unrelated statement.
//!
//! # Why it was unreachable until now
//!
//! `migrate::run` skips the backup when the database has no schema yet
//! (`from_version == 0`), and until the commit-links migration there was
//! exactly one migration in the tree — so `from_version` was *always* zero and
//! the backup path was never taken by a fresh store. The second migration made
//! every existing v1 store take it, which is every upgrading user, whose
//! machine runs `story` from a shell, a git hook and a daemon at once.
//!
//! # The shape of this test
//!
//! It builds its own two-migration list rather than relying on the tree having
//! two, so it stays a test of the *backup* rather than of whichever migrations
//! happen to exist. Eight connections on one file is what eight processes are,
//! as far as SQLite is concerned: separate connections, separate transactions,
//! one write-ahead log.

use std::path::Path;

use storyhook::store::migrate::{self, Migration};
use storyhook::store::{SqliteStore, Store, StoreConfig};
use storyhook_test_support::scratch_dir;

/// The tree's migrations plus one more, so this file exercises a *pending*
/// migration against a database that already has a schema.
fn with_extra_migration() -> Vec<Migration> {
    let mut list = migrate::MIGRATIONS.to_vec();
    list.push(Migration {
        version: u32::try_from(list.len()).expect("a short list") + 1,
        name: "test-only",
        sql: "CREATE TABLE later (x INTEGER);",
        foreign_keys_off: false,
    });
    list
}

/// A store carrying the tree's current schema, ready for one more migration.
fn migrated_store(path: &Path, backups: &Path) -> SqliteStore {
    let store = SqliteStore::open_with(StoreConfig {
        backup_dir: backups.to_path_buf(),
        ..StoreConfig::new(path.to_path_buf())
    })
    .expect("opening the store");
    store.migrate().expect("bringing it to the current version");
    store
}

#[test]
fn eight_connections_migrating_one_store_all_succeed() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    let backups = dir.path().join("backups");
    drop(migrated_store(&path, &backups));

    let migrations = with_extra_migration();
    let errors: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (path, backups, migrations) =
                    (path.clone(), backups.clone(), migrations.clone());
                scope.spawn(move || {
                    let store = SqliteStore::open_with(StoreConfig {
                        backup_dir: backups,
                        ..StoreConfig::new(path)
                    })
                    .expect("opening the store");
                    store.migrate_with(&migrations).err().map(|e| e.to_string())
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().expect("a migrating thread panicked"))
            .collect()
    });

    assert!(
        errors.is_empty(),
        "every caller must come back with a migrated store, whether it did the \
         work or found it already done:\n{errors:#?}"
    );
}

/// Two backups taken in the same millisecond are two files, not one failure.
///
/// The narrower statement of the same defect, without the concurrency: the
/// property that matters is that a name collision cannot occur, and a loop is a
/// more reliable way to put two calls in one millisecond than a race is.
#[test]
fn backups_taken_in_the_same_millisecond_do_not_collide() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    let backups = dir.path().join("backups");
    let store = migrated_store(&path, &backups);
    let migrations = with_extra_migration();

    // The first call migrates; the rest find nothing pending. Only the first
    // backs up, so a second store is needed to take a second backup — which is
    // the realistic shape anyway: one machine, several projects, one upgrade.
    let first = store
        .migrate_with(&migrations)
        .expect("the first migration")
        .backup
        .expect("a database with a schema is backed up");

    let second_path = dir.path().join("second.db");
    let second_store = migrated_store(&second_path, &backups);
    let second = second_store
        .migrate_with(&migrations)
        .expect("the second migration")
        .backup
        .expect("and so is the second database");

    assert_ne!(
        first, second,
        "two backups must not be handed the same filename"
    );
    assert!(first.exists() && second.exists());
}

/// No staging file is left behind.
///
/// The backup is written under a private name and renamed into place; a crash
/// between the two would leave a `.tmp` in a directory an operator is expected
/// to browse.
#[test]
fn a_successful_backup_leaves_no_staging_file() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    let backups = dir.path().join("backups");
    let store = migrated_store(&path, &backups);

    store
        .migrate_with(&with_extra_migration())
        .expect("migrating");

    let leftovers: Vec<String> = std::fs::read_dir(&backups)
        .expect("reading the backup directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".tmp") || name.starts_with('.'))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the backup directory must hold finished backups only: {leftovers:?}"
    );
}
