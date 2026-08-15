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
//! opened, `PRAGMA integrity_check`ed, and asked whether it holds any pages at
//! all before anything is allowed to change. That last question is not
//! redundant: an empty file passes the integrity check, so without it the
//! backup guarantee rested entirely on `VACUUM INTO` having done its job, and
//! verification added no independent evidence that the copy held anything.
//! If any step fails, no migration is attempted at all.

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
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
    ///
    /// **A rebuild of `stories` needs one more thing since schema 5**: drop
    /// `events_reject_delete` at the top of the migration and recreate it at
    /// the bottom. That trigger names `stories`, and `ALTER TABLE … RENAME TO`
    /// re-parses every trigger in the schema — so between the `DROP TABLE` and
    /// the rename there is nothing for it to resolve. `0005_purge_story.sql`
    /// explains it, and `tests/store_migrations.rs` measures both directions.
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
    Migration {
        version: 4,
        name: "state_superstate_agree",
        sql: include_str!("schema/0004_state_superstate_agree.sql"),
        // Rebuilds `stories`, which three tables reference. See the flag's own
        // documentation: with enforcement on, the `DROP TABLE` in there empties
        // every one of them and commits successfully.
        foreign_keys_off: true,
    },
    Migration {
        version: 5,
        name: "purge_story",
        sql: include_str!("schema/0005_purge_story.sql"),
        foreign_keys_off: false,
    },
    Migration {
        version: 6,
        name: "project_remotes",
        sql: include_str!("schema/0006_project_remotes.sql"),
        // Creates one table and one index; rebuilds nothing, so enforcement
        // stays on and migration 5's `events_reject_delete` warning does not
        // apply. Its own header says so where the next author will look.
        foreign_keys_off: false,
    },
    Migration {
        version: 7,
        name: "project_checkout",
        sql: include_str!("schema/0007_project_checkout.sql"),
        // One `ALTER TABLE … ADD COLUMN`; rebuilds nothing, so migration 5's
        // `events_reject_delete` warning does not apply here either.
        foreign_keys_off: false,
    },
    Migration {
        version: 8,
        name: "drop_project_paths",
        sql: include_str!("schema/0008_drop_project_paths.sql"),
        // One `UPDATE` and one `DROP TABLE` of a leaf table — nothing
        // references `project_paths`, and no trigger names it — so nothing is
        // rebuilt and migration 5's warning does not apply.
        foreign_keys_off: false,
    },
    Migration {
        version: 9,
        name: "type_emoji_and_drop_task",
        sql: include_str!("schema/0009_type_emoji_and_drop_task.sql"),
        // `ALTER TABLE ... ADD COLUMN`, appended events, and ordinary
        // `UPDATE`/`DELETE` statements — nothing here rebuilds a table, so
        // migration 5's `events_reject_delete` warning does not apply.
        foreign_keys_off: false,
    },
    Migration {
        version: 10,
        name: "story_hidden",
        sql: include_str!("schema/0010_story_hidden.sql"),
        // One `ALTER TABLE ... ADD COLUMN`, no CHECK, nothing rebuilt — see
        // the migration's own header for why this column deliberately has no
        // CHECK. Migration 5's `events_reject_delete` warning does not apply.
        foreign_keys_off: false,
    },
    Migration {
        version: 11,
        name: "pr_links",
        sql: include_str!("schema/0011_pr_links.sql"),
        // `CREATE TABLE` of a new, unreferenced table — nothing is rebuilt, so
        // migration 5's `events_reject_delete` warning does not apply.
        foreign_keys_off: false,
    },
    Migration {
        version: 12,
        name: "story_draft",
        sql: include_str!("schema/0012_story_draft.sql"),
        // One `ALTER TABLE ... ADD COLUMN`, no CHECK, nothing rebuilt — see
        // the migration's own header for why this column deliberately has no
        // CHECK. Migration 5's `events_reject_delete` warning does not apply.
        foreign_keys_off: false,
    },
    Migration {
        version: 13,
        name: "event_provenance",
        sql: include_str!("schema/0013_event_provenance.sql"),
        // Two `ALTER TABLE ... ADD COLUMN` on `events`, both NULL-able and
        // neither backfilled — see the migration's own header for why the NULLs
        // are the point. Nothing is rebuilt, so migration 5's
        // `events_reject_delete` warning does not apply, and the append-only
        // triggers are untouched: adding a column is not an UPDATE of a row.
        foreign_keys_off: false,
    },
    Migration {
        version: 14,
        name: "commit_scan",
        sql: include_str!("schema/0014_commit_scan.sql"),
        // One `ALTER TABLE ... ADD COLUMN` on `projects`, NULL-able and not
        // backfilled — see the migration's own header for why the NULL is the
        // point and why the column is not a setting. Nothing is rebuilt, so
        // migration 5's `events_reject_delete` warning does not apply.
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

/// Whether two copies taken in the same millisecond under the same label are
/// one file or two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Naming {
    /// One file: the second copy renames over the first.
    ///
    /// Correct precisely when the losers are byte-equivalent — two copies of
    /// one database at one version, which is every backup no write is waiting
    /// on. [`crate::daemon::backup::prune`] keeps the newest few *by name*, so
    /// giving identical content distinct names would evict real history in
    /// order to store duplicates (SH-295).
    Collapsing,
    /// Two files.
    ///
    /// Correct when the copies are of *different* states — which is what a
    /// coupled copy is. [`crate::store::Store::write_with_snapshot`] takes one
    /// per write, so two runs in one millisecond hold the state before the
    /// first rewrite and the state after it. Collapsing those keeps the wrong
    /// one: the survivor already contains the rewrite an operator is trying to
    /// get back from, and the true "before" is gone (SH-297).
    ///
    /// The retention argument above does not apply here. Coupled copies are
    /// written to [`crate::env::Environment::maintenance_backups_dir`], which
    /// the daily prune never scans.
    PerRun,
}

/// Takes a consistent copy of the database and verifies it opens and passes
/// `PRAGMA integrity_check`.
///
/// `label` distinguishes what the copy is *for*, so a directory holding both
/// pre-migration backups and daily snapshots still says which is which.
///
/// Two of these taken in one millisecond under one label are deliberately one
/// file — see [`Naming::Collapsing`]. A copy some write depends on wants
/// [`snapshot_coupled`] instead.
pub(crate) fn snapshot(
    conn: &Connection,
    backup_dir: &Path,
    label: &str,
) -> Result<PathBuf, StoreError> {
    snapshot_at(conn, backup_dir, label, Utc::now(), Naming::Collapsing)
}

/// [`snapshot`], for a copy that a particular write begins from.
///
/// Named per run rather than per millisecond, because two of these are two
/// different states of the database and both have to survive — see
/// [`Naming::PerRun`]. Called only by an implementation of
/// [`crate::store::Store::write_with_snapshot`].
pub(crate) fn snapshot_coupled(
    conn: &Connection,
    backup_dir: &Path,
    label: &str,
) -> Result<PathBuf, StoreError> {
    snapshot_at(conn, backup_dir, label, Utc::now(), Naming::PerRun)
}

/// [`snapshot`] with the clock as a parameter.
///
/// The name a backup gets is a millisecond timestamp, so *the moment* is the
/// only input that decides whether two calls collide — and a test that cannot
/// choose it cannot state this module's central property. Two calls a
/// millisecond apart prove nothing: they are two names, and they were two names
/// before the collision was fixed too. `taken_at` is what lets a test put two
/// backups in one millisecond deliberately rather than hope for it, which is
/// the whole of SH-295: the test that claimed to guard the collision never
/// produced one, and stayed green with the defect reinstated.
///
/// Production has two callers — [`snapshot`] and [`snapshot_coupled`], each
/// passing [`Utc::now`] and its own [`Naming`].
pub(crate) fn snapshot_at(
    conn: &Connection,
    backup_dir: &Path,
    label: &str,
    taken_at: DateTime<Utc>,
    naming: Naming,
) -> Result<PathBuf, StoreError> {
    std::fs::create_dir_all(backup_dir).map_err(|e| {
        StoreError::Backup(format!(
            "could not create the backup directory {}: {e}",
            backup_dir.display()
        ))
    })?;

    let stamp = taken_at.format("%Y%m%dT%H%M%S%.3fZ");
    // Unique across processes *and* threads: a process id, and a counter for
    // the two threads of one process that can reach here in the same
    // millisecond.
    let run = format!(
        "{}-{}",
        std::process::id(),
        STAGING_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let path = match naming {
        Naming::Collapsing => backup_dir.join(format!("storyhook-{stamp}-{label}.db")),
        Naming::PerRun => backup_dir.join(format!("storyhook-{stamp}-{label}-{run}.db")),
    };
    // Written to a private name and renamed into place, never straight to
    // `path`. `VACUUM INTO` **refuses an output file that already exists**, and
    // a `Collapsing` name is a millisecond timestamp, so every process upgrading
    // one store at one moment computes the same one — eight of them racing
    // produce one backup and seven failed migrations. Worse, SQLite reports the
    // collision through a stale `sqlite3_errmsg` ("table schema_migrations
    // already exists"), which names the wrong problem entirely.
    //
    // The staging name always carries `run`, so it is private to this call
    // whichever naming the final file uses. Under `Collapsing` the rename is
    // atomic and last-writer-wins, and the losers are byte-equivalent snapshots
    // of the same database at the same version, so which one survives does not
    // matter; under `PerRun` there is no loser, because no two calls compute
    // the same final name.
    let staging = backup_dir.join(format!(".storyhook-{stamp}-{label}-{run}.tmp"));

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

/// How many pages a database holds.
///
/// Zero means the file was created but never written to — the one state
/// [`verify`] cannot learn about from `PRAGMA integrity_check`.
fn page_count(conn: &Connection) -> Result<i64, rusqlite::Error> {
    conn.query_row("PRAGMA page_count", [], |row| row.get(0))
}

/// Opens a backup and asserts SQLite considers it sound **and that it holds
/// something**.
///
/// Soundness alone is not evidence of a backup: `PRAGMA integrity_check`
/// answers `ok` for a pageless database, because a fresh, empty file genuinely
/// is one. So a file that never received a copy passed this function, and
/// [`snapshot_at`] renamed it over the real one and returned `Ok` (SH-296).
/// `PRAGMA page_count` closes that, and it is the check to make here for a
/// reason worth writing down, because the obvious stronger one is unsound:
///
/// - **It cannot produce a false failure.** `VACUUM INTO` writes a one-page
///   database even from a source with no pages, so every genuine copy holds at
///   least one page. `a_backup_of_a_pageless_database_still_verifies` pins that.
/// - **Comparing the copy against the source cannot be made race-free.**
///   Matching the copy's `user_version` to the source connection's would also
///   catch a copy of the *wrong* database — but `VACUUM INTO` refuses to run
///   inside a transaction, so no read snapshot can span the copy and the
///   source read. A second process committing a migration in that window (the
///   race [`apply`] exists to survive, and `two_backups_in_one_millisecond_
///   collapse_to_one_verified_file` provokes) would fail a backup that is
///   perfectly good, aborting a migration that was safe.
/// - Comparing page **counts** is wrong outright: `VACUUM INTO` compacts, so a
///   sound copy legitimately holds fewer pages than its source.
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
    // Asked only of a file SQLite has already called sound, so the answer is a
    // statement about a database rather than about arbitrary bytes.
    let pages = page_count(&copy)
        .map_err(|e| StoreError::Backup(format!("{} could not be sized: {e}", path.display())))?;
    if pages == 0 {
        return Err(StoreError::Backup(format!(
            "{} holds no pages: it is a database, but not a copy of one",
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
        // Derived rather than written down: the interesting property is that
        // the list is contiguous and that the reported version is its last
        // entry, not what today's number happens to be. A hard-coded literal
        // here is a test that fails for the one reason that is never a defect.
        assert_eq!(
            current_schema_version(),
            u32::try_from(MIGRATIONS.len()).expect("a short list")
        );
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

    /// A database with a schema, for the backup tests below.
    fn a_database(dir: &Path) -> Connection {
        let conn = Connection::open(dir.join("store.db")).expect("opening a database");
        conn.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
            .expect("giving it something to copy");
        conn
    }

    /// The instant from the original bug report's error message, reused so the
    /// two backups below are computed at one moment by construction rather than
    /// by luck.
    fn one_moment() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-29T09:22:28.049Z")
            .expect("a valid instant")
            .with_timezone(&Utc)
    }

    /// Asserts `path` is a copy of [`a_database`] rather than merely a file
    /// SQLite tolerates.
    ///
    /// [`verify`] is not enough on its own, and that is worth stating in code:
    /// `PRAGMA integrity_check` answers `ok` for an **empty** database, so a
    /// backup of zero bytes passes it. A test that asks only whether the file
    /// verifies cannot tell a backup from a file that never received one — a
    /// failure this module produced under experiment, when a copy written to
    /// the wrong name left the empty staging database standing in for it.
    fn holds_the_copied_row(path: &Path) {
        let copy = Connection::open(path).expect("reopening the backup");
        let x: i64 = copy
            .query_row("SELECT x FROM t", [], |row| row.get(0))
            .expect("the backup must contain the source database's contents");
        assert_eq!(x, 1, "the backup's contents must be the source's");
    }

    /// Names that leave no `.tmp` or dotfile behind.
    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("reading the backup directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".tmp") || name.starts_with('.'))
            .collect()
    }

    /// Two backups computed in one millisecond are one verified file — not one
    /// failure, and not two files.
    ///
    /// This is the deterministic statement of the defect
    /// `tests/store_backup_naming.rs` documents: a backup's name is a
    /// millisecond timestamp handed to `VACUUM INTO`, which **refuses an output
    /// file that already exists**, so two callers naming one file at one moment
    /// produced one backup and one failed migration — reported through a stale
    /// `sqlite3_errmsg` that named the wrong problem.
    ///
    /// **Both calls must succeed, and both must return the same path.** The
    /// collision condition *is* a shared name: the stamp and the label are the
    /// whole of it, so two calls that share both cannot produce two files. That
    /// is deliberate, and it is what [`snapshot_at`]'s rename buys — the losers
    /// are byte-equivalent copies of one database, and `daemon::backup::prune`
    /// keeps the newest few *by name*, so distinct names for identical content
    /// would evict real history to store duplicates.
    ///
    /// The test this replaces asserted the opposite — that the two paths
    /// *differ* — and could only pass by never producing the collision it named
    /// (SH-295). It migrated a second store between the two backups, which costs
    /// more than a millisecond, so the names differed for a reason unrelated to
    /// the property; with the defect reinstated it stayed green. Had it ever met
    /// its own premise it would have failed, because both of its backups carried
    /// the same label too.
    #[test]
    fn two_backups_in_one_millisecond_collapse_to_one_verified_file() {
        let dir = storyhook_test_support::scratch_dir();
        let conn = a_database(dir.path());
        let backups = dir.path().join("backups");

        let first = snapshot_at(&conn, &backups, "v1", one_moment(), Naming::Collapsing)
            .expect("the first backup");
        let second = snapshot_at(&conn, &backups, "v1", one_moment(), Naming::Collapsing)
            .expect("the second backup, whose name the first already holds");

        assert_eq!(
            first, second,
            "one stamp and one label are one name, so the second backup renames \
             over the first rather than becoming a second file"
        );
        assert!(first.exists(), "the surviving backup must be on disk");
        verify(&first).expect("and must still be a sound database");
        holds_the_copied_row(&first);
        assert!(
            leftovers(&backups).is_empty(),
            "no staging file may survive either call: {:?}",
            leftovers(&backups)
        );
    }

    /// Two *coupled* copies in one millisecond are two files, because they are
    /// copies of two different states.
    ///
    /// The exception the property above is built on. Collapsing is justified
    /// entirely by the losers being byte-equivalent, and that premise is false
    /// the moment a copy is tied to a write: `write_with_snapshot` takes one
    /// per write, so if two `set-prefix` runs land in one millisecond the
    /// second copy already contains the first's rewrite. Under one name the
    /// survivor is that second copy, and the state an operator would be trying
    /// to get back to — before either rewrite — is gone (SH-297).
    ///
    /// Stated here rather than in an integration test for the reason
    /// [`snapshot_at`]'s own doc gives: the moment decides, and only a caller
    /// that chooses the moment can produce the collision rather than hope for
    /// it.
    #[test]
    fn two_coupled_copies_in_one_millisecond_are_two_files() {
        let dir = storyhook_test_support::scratch_dir();
        let conn = a_database(dir.path());
        let backups = dir.path().join("backups");

        let first = snapshot_at(&conn, &backups, "set-prefix", one_moment(), Naming::PerRun)
            .expect("the first coupled copy");
        let second = snapshot_at(&conn, &backups, "set-prefix", one_moment(), Naming::PerRun)
            .expect("the second coupled copy, taken in the same millisecond");

        assert_ne!(
            first, second,
            "two copies of two different states must not share a name: the second \
             would rename over the first, and the true 'before' state would be gone"
        );
        assert!(first.exists(), "the first coupled copy must survive");
        assert!(second.exists(), "and so must the second");
        verify(&first).expect("the first must be a sound database");
        verify(&second).expect("and so must the second");
        holds_the_copied_row(&first);
        holds_the_copied_row(&second);
        assert!(
            leftovers(&backups).is_empty(),
            "no staging file may survive either call: {:?}",
            leftovers(&backups)
        );
    }

    /// The label is part of the name, so two *different* backups taken at one
    /// instant are two files.
    ///
    /// The other half of the naming contract [`snapshot`] documents: one
    /// directory holds both pre-migration backups and the daemon's daily
    /// snapshots, and an operator must be able to tell which is which. Without
    /// this, a collapse into one file — the property above — could be satisfied
    /// by ignoring the label entirely.
    #[test]
    fn two_labels_at_one_instant_are_two_files() {
        let dir = storyhook_test_support::scratch_dir();
        let conn = a_database(dir.path());
        let backups = dir.path().join("backups");

        let migration = snapshot_at(&conn, &backups, "v1", one_moment(), Naming::Collapsing)
            .expect("the pre-migration backup");
        let daily = snapshot_at(
            &conn,
            &backups,
            "snapshot",
            one_moment(),
            Naming::Collapsing,
        )
        .expect("the daily snapshot");

        assert_ne!(
            migration, daily,
            "backups taken for different reasons must not overwrite each other"
        );
        holds_the_copied_row(&migration);
        holds_the_copied_row(&daily);
    }

    /// A file that never received a backup must not verify as one.
    ///
    /// `PRAGMA integrity_check` answers `ok` for a pageless database — SQLite
    /// considers a fresh, empty file perfectly sound — so soundness alone
    /// cannot tell a copy from a file nothing ever wrote to. This is the
    /// sequence that produced the finding (SH-296), under an experiment that
    /// pointed `VACUUM INTO` at the final name while [`snapshot_at`]'s staging
    /// machinery stayed: `Connection::open` **created** the staging file,
    /// empty; `verify` passed it; the rename put those zero bytes over the real
    /// copy; and `snapshot` returned `Ok` for a backup of nothing.
    #[test]
    fn a_pageless_database_does_not_verify_as_a_backup() {
        let dir = storyhook_test_support::scratch_dir();
        let empty = dir.path().join("never-written.db");
        std::fs::write(&empty, b"").expect("a zero-byte file");

        let error = verify(&empty).expect_err("a file of no pages is not a backup");
        assert!(
            error.to_string().contains("no pages"),
            "the message must say what is wrong with it: {error}"
        );
    }

    /// The emptiness check is a property of the **copy**, so it can never
    /// refuse a genuine one — not even a copy of a database that is itself
    /// pageless.
    ///
    /// This is the false-positive half of the test above, and the reason
    /// `page_count > 0` was chosen over comparing the copy against the source.
    /// `VACUUM INTO` writes a one-page database even when the source has no
    /// pages at all, so "the copy holds pages" separates a real backup from an
    /// unwritten file exactly, with no case in between. Measured on the bundled
    /// SQLite 3.51.0; if a future version ever wrote a pageless copy instead,
    /// this fails here rather than turning every backup in the field into a
    /// refused migration.
    #[test]
    fn a_backup_of_a_pageless_database_still_verifies() {
        let dir = storyhook_test_support::scratch_dir();
        let conn = Connection::open(dir.path().join("store.db")).expect("opening a database");
        assert_eq!(
            page_count(&conn).expect("reading the source's page count"),
            0,
            "the premise: a database with nothing in it has no pages"
        );

        let backup = snapshot(&conn, &dir.path().join("backups"), "v1")
            .expect("a copy of an empty database is still a copy");
        verify(&backup).expect("and must verify");
    }
}
