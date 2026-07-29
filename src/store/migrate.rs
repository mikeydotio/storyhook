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
}

/// Every migration this binary knows how to apply, in order.
///
/// Embedded in the binary rather than read from disk: a migration that can go
/// missing is a migration that can half-apply.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial",
    sql: include_str!("schema/0001_initial.sql"),
}];

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
        // otherwise would make a concurrent `story init` look like it migrated
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
/// failed: table schema_migrations already exists` — exit 5, on a `story init`
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

    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| fail(e.to_string()))?;

    match schema_version(conn) {
        Ok(version) if version >= migration.version => {
            conn.execute_batch("COMMIT")
                .map_err(|e| fail(e.to_string()))?;
            return Ok(false);
        }
        Ok(_) => {}
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
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

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .map_err(|e| fail(e.to_string()))?;
            Ok(true)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
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

    // `VACUUM INTO` goes through SQLite, so it sees a consistent snapshot
    // including anything still in the write-ahead log. `fs::copy` does not.
    conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
        .map_err(|e| StoreError::Backup(format!("could not write {}: {e}", path.display())))?;

    verify(&path)?;
    Ok(path)
}

/// The pre-migration backup: a snapshot labelled with the version it was taken
/// from.
fn back_up(conn: &Connection, backup_dir: &Path, from_version: u32) -> Result<PathBuf, StoreError> {
    snapshot(conn, backup_dir, &format!("v{from_version}"))
}

/// Opens a backup and asserts SQLite considers it sound.
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
        assert_eq!(current_schema_version(), 1);
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
