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
//! It chooses the SQL of the step it migrates *with*, so it stays a test of the
//! *backup* rather than of whichever migration the tree happens to have added
//! last — but it takes that step **in place of** the tree's final migration
//! rather than after it, so the version it produces is one this binary
//! understands (SH-293; the helper below says why that matters). Eight
//! connections on one file is what eight processes are, as far as SQLite is
//! concerned: separate connections, separate transactions, one write-ahead log.
//!
//! # Where the deterministic statement lives
//!
//! **In `src/store/migrate.rs`'s own unit tests**, not here:
//! `two_backups_in_one_millisecond_collapse_to_one_verified_file`. The naming
//! collision is decided by the moment a backup is taken, and no integration
//! test can choose that moment — `migrate_with` and `Store::snapshot` both read
//! the clock themselves, so a test on this side of the crate boundary can only
//! *hope* two calls share a millisecond. This file used to hold a test that
//! hoped, and it never once succeeded: it migrated a second store between its
//! two backups, which costs more than a millisecond, so it stayed green with
//! the defect fully reinstated (SH-295). The unit test passes one instant to
//! `snapshot_at` twice, and is true by construction. What remains here is what
//! only an integration test can say: eight real connections, racing.

use std::path::Path;

use storyhook::store::migrate::{self, Migration};
use storyhook::store::{SqliteStore, StoreConfig};
use storyhook_test_support::scratch_dir;

/// Every migration the tree has but its last — the schema a store carries when
/// exactly one migration is pending.
fn all_but_the_last_migration() -> Vec<Migration> {
    let (_last, rest) = migrate::MIGRATIONS
        .split_last()
        .expect("the tree has migrations");
    rest.to_vec()
}

/// [`all_but_the_last_migration`] with a controlled step put back on the end, so
/// this file exercises a *pending* migration, whose SQL it chose, against a
/// database that already has a schema.
///
/// The step replaces the tree's last migration rather than following it, and
/// that is load-bearing (SH-293). Appending at `MIGRATIONS.len() + 1` — which
/// this file did until then — leaves the database one version *past*
/// [`migrate::current_schema_version`], and `SqliteStore::open_with` refuses
/// such a database with `SchemaTooNew`. That refusal is correct: it is what
/// stops an older binary from touching a store a newer one has upgraded. It is
/// simply not something a caller in this file can survive, so the eight-way race
/// below was green only while every connection managed to open before the winner
/// committed — a race it does not control. It won that race 8 times out of 8 run
/// alone, and lost it, six connections of eight, inside a full `make test` on a
/// machine at load average 39–46. Replacing the final step keeps the SQL chosen
/// here and keeps every version this file writes within what this binary opens.
fn with_the_last_migration_replaced() -> Vec<Migration> {
    let mut list = all_but_the_last_migration();
    list.push(Migration {
        version: u32::try_from(list.len()).expect("a short list") + 1,
        name: "test-only",
        sql: "CREATE TABLE later (x INTEGER);",
        foreign_keys_off: false,
    });
    list
}

/// A store carrying every migration but the last, ready for one more.
fn migrated_store(path: &Path, backups: &Path) -> SqliteStore {
    let store = SqliteStore::open_with(StoreConfig {
        backup_dir: backups.to_path_buf(),
        ..StoreConfig::new(path.to_path_buf())
    })
    .expect("opening the store");
    store
        .migrate_with(&all_but_the_last_migration())
        .expect("bringing it to one step short of the current version");
    store
}

#[test]
fn eight_connections_migrating_one_store_all_succeed() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    let backups = dir.path().join("backups");
    drop(migrated_store(&path, &backups));

    let migrations = with_the_last_migration_replaced();
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

    // The store the eight of them produced must be one this binary can still
    // open — the deterministic statement of SH-293. This file used to migrate
    // *past* `current_schema_version()`, so a straggler whose open landed after
    // the winner's commit was refused with `SchemaTooNew`: correct product
    // behaviour, arriving on a test that had made it unreachable only by
    // winning a race it did not control. Rare, load-sensitive, and it read as a
    // store-layer regression in whichever unrelated branch was being gated.
    drop(
        SqliteStore::open_with(StoreConfig {
            backup_dir: backups.clone(),
            ..StoreConfig::new(path.clone())
        })
        .expect("the store the race produced must still open"),
    );
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
        .migrate_with(&with_the_last_migration_replaced())
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
