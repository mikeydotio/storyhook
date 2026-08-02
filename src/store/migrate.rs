//! Versioned schema migrations, with a verified pre-migration backup.
//!
//! Two failure modes shaped this module.
//!
//! The first is the one that produced version 0: the legacy archive database
//! was created by `CREATE TABLE IF NOT EXISTS` with no migration framework
//! behind it, and the one column ever added to it arrived by an unversioned
//! `ALTER`. Nothing recorded what a given file's shape was, so nothing could
//! tell an old file from a new one. Here the version is recorded twice — in
//! `schema_migrations` for the history, and in `PRAGMA user_version` for the
//! gate — and the gate is checked before any statement that assumes a shape.
//!
//! The second is backups. Copying a SQLite database with `fs::copy` while its
//! write-ahead log is hot produces a file that looks fine and restores
//! corrupt — a backup that fails exactly when it is needed. `VACUUM INTO`
//! takes a consistent snapshot through SQLite itself, and the copy is then
//! opened and `PRAGMA integrity_check`ed before anything is allowed to change.
//! If either step fails, no migration is attempted at all.

use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use rusqlite::Connection;

use crate::store::error::StoreError;
use crate::store::fault::{FaultPoint, fire};
use crate::store::types::MigrationReport;

/// One versioned step from the schema at `version - 1` to the schema at
/// `version`.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    /// The version this migration produces. Must be contiguous from 1.
    pub version: u32,
    /// A short name, recorded in `schema_migrations` and in error messages.
    pub name: &'static str,
    /// The SQL to run, as one batch, inside this migration's transaction.
    pub sql: &'static str,
    /// Whether this migration rebuilds a table other tables reference, and so
    /// must run with foreign keys **disabled**.
    ///
    /// SQLite has no `ALTER TABLE ADD CONSTRAINT`, so adding one means the
    /// twelve-step rebuild: create a replacement, copy, `DROP TABLE` the
    /// original, rename. With foreign keys **on**, that `DROP TABLE` fires
    /// every child's `ON DELETE CASCADE` — and the damage is silent. Measured
    /// on the bundled SQLite 3.51.0: rebuilding `stories` that way empties
    /// `story_labels` and reports a successful COMMIT.
    ///
    /// `PRAGMA defer_foreign_keys` does not help — it defers *checks*, not the
    /// cascade — and the pragma cannot be changed inside a transaction, so
    /// [`apply`] has to set it before `BEGIN` and restore it after. The price of
    /// switching enforcement off is paid back by `PRAGMA foreign_key_check`,
    /// which [`apply`] runs **inside** the transaction: a migration that leaves
    /// one dangling reference rolls back rather than committing.
    ///
    /// Default `false`, and it should stay that way for every migration that
    /// does not rebuild a referenced table.
    pub foreign_keys_off: bool,
}

/// Every migration this binary knows how to apply, in order.
///
/// Embedded in the binary rather than read from disk: a migration that can go
/// missing is a migration that can half-apply.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("schema/0001_initial.sql"),
        foreign_keys_off: false,
    },
    Migration {
        version: 2,
        name: "commit_links",
        sql: include_str!("schema/0002_commit_links.sql"),
        foreign_keys_off: false,
    },
    Migration {
        version: 3,
        name: "delete_project",
        sql: include_str!("schema/0003_delete_project.sql"),
        foreign_keys_off: false,
    },
];

/// The newest schema version this binary understands.
#[must_use]
pub fn current_schema_version() -> u32 {
    MIGRATIONS.last().map_or(0, |m| m.version)
}

/// Reads the schema version recorded in a database.
///
/// `PRAGMA user_version` is the authority rather than `MAX(schema_migrations
/// .version)`, deliberately: a database written by a much newer storyhook may
/// have renamed or restructured `schema_migrations` itself, and the whole point
/// of the gate is to give a clear answer in exactly that case.
pub fn schema_version(conn: &Connection) -> Result<u32, StoreError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| StoreError::from_sqlite(e, "reading the schema version"))?;
    u32::try_from(version).map_err(|_| {
        StoreError::Corrupt(format!("user_version holds a negative value ({version})"))
    })
}

/// Fails if the database is newer than this binary understands.
///
/// The SH-54 gate. A forward-incompatible database used to surface as a serde
/// or rusqlite error from somewhere deep inside a read; now it surfaces once,
/// here, as a sentence that names the problem and the remedy.
pub fn ensure_readable(conn: &Connection, supported: u32) -> Result<(), StoreError> {
    let found = schema_version(conn)?;
    if found > supported {
        return Err(StoreError::SchemaTooNew { found, supported });
    }
    Ok(())
}

/// Applies every pending migration in `migrations`, backing the database up
/// first.
///
/// Each migration runs in its own transaction and records itself in
/// `schema_migrations` and in `PRAGMA user_version` within that same
/// transaction, so a failure leaves the database at a version that is true.
pub fn run(
    conn: &Connection,
    migrations: &[Migration],
    backup_dir: &Path,
) -> Result<MigrationReport, StoreError> {
    validate_sequence(migrations);

    let from_version = schema_version(conn)?;
    let supported = migrations.last().map_or(0, |m| m.version);
    if from_version > supported {
        return Err(StoreError::SchemaTooNew {
            found: from_version,
            supported,
        });
    }

    let pending: Vec<&Migration> = migrations
        .iter()
        .filter(|m| m.version > from_version)
        .collect();
    if pending.is_empty() {
        return Ok(MigrationReport {
            from_version,
            to_version: from_version,
            applied: Vec::new(),
            backup: None,
        });
    }

    // Nothing to lose backing up a database that has no schema yet. Every
    // other case gets a verified copy before a single statement runs.
    let backup = if from_version == 0 {
        None
    } else {
        Some(back_up(conn, backup_dir, from_version)?)
    };

    let mut applied = Vec::with_capacity(pending.len());
    for migration in pending {
        fire(FaultPoint::MidMigration)?;
        // A migration another process applied first is not this one's work to
        // report: `applied` is what *this* call changed, and a report claiming
        // otherwise would make a concurrent `story project init` look like it migrated
        // a database it merely opened.
        if apply(conn, migration)? {
            applied.push(migration.name.to_string());
        }
    }

    Ok(MigrationReport {
        from_version,
        to_version: schema_version(conn)?,
        applied,
        backup,
    })
}

/// Panics if the migration list is not contiguous and ascending from 1.
///
/// A programming error in a `const` this crate owns, caught the first time
/// anything migrates rather than the first time a gap matters. Contiguity is
/// what lets `user_version` alone decide what is pending.
fn validate_sequence(migrations: &[Migration]) {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = u32::try_from(index + 1).expect("migration list fits in u32");
        assert_eq!(
            migration.version, expected,
            "migrations must be contiguous and ascending from 1; found version {} at position {}",
            migration.version, index
        );
    }
}

/// Applies one migration, reporting whether it had anything to do.
///
/// The version is re-read **inside** the transaction, and that is the whole of
/// the concurrency story. `run` decides what is pending outside any
/// transaction, so two processes opening a fresh store both see version 0 and
/// both queue migration 1; without this check the loser's `CREATE TABLE
/// schema_migrations` fails and the user gets `error: migration 1 (initial)
/// failed: table schema_migrations already exists` — exit 5, on a `story project init`
/// that did nothing wrong. `BEGIN IMMEDIATE` serializes the two; re-reading
/// under it is what lets the second one notice it has already been overtaken.
///
/// Found by the store test leg: a parallel run is dozens of processes racing to
/// create one database, which is the same shape as a hook shelling out to
/// `story` while its parent still holds the store.
fn apply(conn: &Connection, migration: &Migration) -> Result<bool, StoreError> {
    let fail = |detail: String| StoreError::Migration {
        version: migration.version,
        name: migration.name.to_string(),
        detail,
    };

    // Outside the transaction on purpose: SQLite silently ignores `PRAGMA
    // foreign_keys` inside one. Restored in every exit path below, because the
    // caller hands the same connection to the next migration and then to the
    // store itself — leaving enforcement off would disarm it for the process.
    if migration.foreign_keys_off {
        conn.pragma_update(None, "foreign_keys", false)
            .map_err(|e| fail(format!("disabling foreign keys: {e}")))?;
    }
    let restore_foreign_keys = |conn: &Connection| {
        if migration.foreign_keys_off {
            let _ = conn.pragma_update(None, "foreign_keys", true);
        }
    };

    if let Err(error) = conn.execute_batch("BEGIN IMMEDIATE") {
        restore_foreign_keys(conn);
        return Err(fail(error.to_string()));
    }

    match schema_version(conn) {
        Ok(version) if version >= migration.version => {
            let committed = conn.execute_batch("COMMIT");
            restore_foreign_keys(conn);
            committed.map_err(|e| fail(e.to_string()))?;
            return Ok(false);
        }
        Ok(_) => {}
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            restore_foreign_keys(conn);
            return Err(error);
        }
    }

    let result = (|| -> Result<(), rusqlite::Error> {
        conn.execute_batch(migration.sql)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                migration.version,
                migration.name,
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
            ],
        )?;
        // `PRAGMA user_version` takes a literal, not a parameter. The value is
        // a `u32` from a const in this crate, so there is no injection surface.
        conn.execute_batch(&format!("PRAGMA user_version = {}", migration.version))?;
        Ok(())
    })();

    // What buys back the enforcement switched off above, and the reason it is
    // safe to switch off at all. `foreign_key_check` reports every dangling
    // reference in the database; inside the transaction, so a migration that
    // creates one rolls back instead of committing silently. Runs only for the
    // migrations that asked, since it is a full scan of every child table.
    let result = result.and_then(|()| {
        if !migration.foreign_keys_off {
            return Ok(());
        }
        let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
        let violations: Vec<String> = stmt
            .query_map([], |row| {
                let table: String = row.get(0)?;
                let parent: String = row.get(2)?;
                Ok(format!("{table} -> {parent}"))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if violations.is_empty() {
            Ok(())
        } else {
            // A rusqlite error so it joins the same rollback path as any other
            // failing statement; the message names what dangles.
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "left {} dangling foreign-key reference(s): {}",
                    violations.len(),
                    violations.join(", ")
                )),
            ))
        }
    });

    match result {
        Ok(()) => {
            let committed = conn.execute_batch("COMMIT");
            restore_foreign_keys(conn);
            committed.map_err(|e| fail(e.to_string()))?;
            Ok(true)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            restore_foreign_keys(conn);
            Err(fail(error.to_string()))
        }
    }
}

/// Takes a consistent copy of the database and verifies it opens and passes
/// `PRAGMA integrity_check`.
///
/// `label` distinguishes what the copy is *for*, so a directory holding both
/// pre-migration backups and daily snapshots still says which is which.
pub(crate) fn snapshot(
    conn: &Connection,
    backup_dir: &Path,
    label: &str,
) -> Result<PathBuf, StoreError> {
    std::fs::create_dir_all(backup_dir).map_err(|e| {
        StoreError::Backup(format!(
            "could not create the backup directory {}: {e}",
            backup_dir.display()
        ))
    })?;

    let stamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let path = backup_dir.join(format!("storyhook-{stamp}-{label}.db"));
    // Written to a private name and renamed into place, never straight to
    // `path`. `VACUUM INTO` **refuses an output file that already exists**, and
    // the name is a millisecond timestamp, so every process upgrading one store
    // at one moment computes the same one — eight of them racing produce one
    // backup and seven failed migrations. Worse, SQLite reports the collision
    // through a stale `sqlite3_errmsg` ("table schema_migrations already
    // exists"), which names the wrong problem entirely.
    //
    // The staging name carries the process id and a per-process counter, so it
    // is unique across processes *and* threads. The rename is atomic and
    // last-writer-wins: the losers are byte-equivalent snapshots of the same
    // database at the same version, so which one survives does not matter.
    let staging = backup_dir.join(format!(
        ".storyhook-{stamp}-{label}-{}-{}.tmp",
        std::process::id(),
        STAGING_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));

    // `VACUUM INTO` goes through SQLite, so it sees a consistent snapshot
    // including anything still in the write-ahead log. `fs::copy` does not.
    let written = conn
        .execute("VACUUM INTO ?1", [staging.to_string_lossy().as_ref()])
        .map_err(|e| StoreError::Backup(format!("could not write {}: {e}", path.display())));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&staging);
        return Err(error);
    }

    if let Err(error) = verify(&staging) {
        let _ = std::fs::remove_file(&staging);
        return Err(error);
    }

    std::fs::rename(&staging, &path).map_err(|e| {
        let _ = std::fs::remove_file(&staging);
        StoreError::Backup(format!("could not write {}: {e}", path.display()))
    })?;
    Ok(path)
}

/// Distinguishes two backups taken by one process in the same millisecond.
///
/// The process id alone is not enough: `make test` runs many tests in threads
/// inside one binary, and two of them can migrate two stores at once.
static STAGING_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The pre-migration backup: a snapshot labelled with the version it was taken
/// from.
fn back_up(conn: &Connection, backup_dir: &Path, from_version: u32) -> Result<PathBuf, StoreError> {
    snapshot(conn, backup_dir, &format!("v{from_version}"))
}

/// Opens a backup and asserts SQLite considers it sound.
///
/// The message names `path` — which, when [`snapshot`] calls it, is the staging
/// name rather than the final one. That is deliberate: the file the operator
/// would go and look at is the one that exists.
fn verify(path: &Path) -> Result<(), StoreError> {
    fire(FaultPoint::BackupVerify)
        .map_err(|e| StoreError::Backup(format!("{} did not verify: {e}", path.display())))?;

    let copy = Connection::open(path).map_err(|e| {
        StoreError::Backup(format!("{} could not be reopened: {e}", path.display()))
    })?;
    let verdict: String = copy
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| {
            StoreError::Backup(format!(
                "{} could not be integrity-checked: {e}",
                path.display()
            ))
        })?;
    if verdict != "ok" {
        return Err(StoreError::Backup(format!(
            "{} failed its integrity check: {verdict}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::inverse_relation;

    #[test]
    fn the_embedded_migration_list_is_contiguous() {
        validate_sequence(MIGRATIONS);
        assert_eq!(current_schema_version(), 3);
    }

    /// The mirror triggers look inverses up in `relation_inverses`, so that
    /// table is a second home for a mapping `domain` already owns. This is the
    /// test that stops the two from drifting: a relation added to the domain
    /// without a matching seed row would leave the store rejecting it.
    #[test]
    fn relation_vocabulary_matches_domain() {
        let sql = MIGRATIONS[0].sql;
        for relation in [
            "relates-to",
            "blocks",
            "blocked-by",
            "parent-of",
            "child-of",
            "duplicate-of",
            "obviates",
            "obviated-by",
        ] {
            let inverse = inverse_relation(relation).expect("domain knows this relation");
            assert!(
                sql.contains(&format!("('{relation}',")),
                "schema is missing a seed row for `{relation}`"
            );
            assert!(
                sql.contains(&format!("'{inverse}')")),
                "schema is missing the inverse `{inverse}`"
            );
        }
    }

    /// File-backed, like every other test in this module and in the
    /// conformance suite: `:memory:` databases have no write-ahead log, no
    /// reopen, and no crash — the three things the store's guarantees are
    /// about.
    #[test]
    fn a_negative_user_version_is_reported_as_corruption() {
        let dir = storyhook_test_support::scratch_dir();
        let conn = Connection::open(dir.path().join("store.db")).unwrap();
        conn.execute_batch("PRAGMA user_version = -1").unwrap();
        let error = schema_version(&conn).unwrap_err();
        assert!(error.to_string().contains("negative"), "{error}");
    }
}
