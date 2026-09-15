//! Recognizes released schema histories before crossing their version collision.
use super::{MIGRATIONS, schema_version};
use crate::store::StoreError;
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension};

const LANDING: &[&str] = &["id", "project_id", "story_no", "payload"];
const NATIVE: &[&str] = &["project_id", "story_no", "reservation"];
const CARD: &[&str] = &["project_id", "story_no", "token", "record_json"];

pub(super) fn has_columns(
    conn: &Connection,
    table: &str,
    expected: &[&str],
) -> Result<bool, StoreError> {
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?
        .query_map([table], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if columns.is_empty() {
        return Ok(false);
    }
    if expected
        .iter()
        .all(|column| columns.iter().any(|name| name == column))
    {
        Ok(true)
    } else {
        Err(StoreError::Corrupt(format!(
            "schema table `{table}` has columns {columns:?}; expected {expected:?}"
        )))
    }
}

fn expect_table(
    conn: &Connection,
    table: &str,
    columns: &[&str],
    present: bool,
) -> Result<(), StoreError> {
    let actual = has_columns(conn, table, columns)?;
    if actual && present {
        validate_definition(conn, table)?;
    }
    if actual == present {
        Ok(())
    } else {
        Err(StoreError::Corrupt(format!(
            "schema lineage disagrees with table `{table}`: expected {}, found {}",
            if present { "present" } else { "absent" },
            if actual { "present" } else { "absent" }
        )))
    }
}

fn history(conn: &Connection) -> Result<Vec<(u32, String, String)>, StoreError> {
    Ok(conn
        .prepare("SELECT version,name,applied_at FROM schema_migrations ORDER BY version")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?)
}

/// Inspection holds a read snapshot so a concurrent bridge cannot mix histories.
pub(super) fn inspect(conn: &Connection) -> Result<bool, StoreError> {
    conn.execute_batch("BEGIN")?;
    let result = classify(conn);
    match result {
        Ok(main) => {
            conn.execute_batch("COMMIT")?;
            Ok(main)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn classify(conn: &Connection) -> Result<bool, StoreError> {
    let version = schema_version(conn)?;
    if version == 0 {
        return Ok(false);
    }
    let recorded = history(conn)?;
    if recorded.len() != version as usize {
        return Err(StoreError::Corrupt(format!(
            "schema version {version} disagrees with {} migration records",
            recorded.len()
        )));
    }
    let main = (version == 38 || version == 39)
        && recorded
            .get(37)
            .is_some_and(|(_, name, _)| name == "landing_intents");
    for (index, (number, name, _)) in recorded.iter().take(MIGRATIONS.len()).enumerate() {
        let expected = if main && index == 37 {
            "landing_intents"
        } else if main && index == 38 {
            "story_reset"
        } else {
            MIGRATIONS[index].name
        };
        if *number != index as u32 + 1 || name != expected {
            return Err(StoreError::Corrupt(format!(
                "unrecognized schema lineage at version {number}: `{name}`, expected `{expected}`"
            )));
        }
    }
    expect_table(conn, "dropped_cleanups", CARD, version >= 45)?;
    expect_table(conn, "landing_intents", LANDING, main || version >= 44)?;
    expect_table(conn, "story_reset_reservations", NATIVE, version >= 44)?;
    if main && version == 39 {
        expect_table(conn, "story_resets", NATIVE, true)?;
    } else {
        expect_table(conn, "story_resets", CARD, version >= 43)?;
    }
    for (minimum, table, columns) in [
        (
            38,
            "block_deliveries",
            &[
                "id",
                "project_id",
                "story_no",
                "action",
                "status",
                "target",
                "detail",
            ][..],
        ),
        (39, "verification_recovery", &["project_id", "receipt"][..]),
        (
            41,
            "continuations",
            &["id", "project_id", "story_no", "revision", "record"][..],
        ),
        (
            42,
            "engine_resets",
            &[
                "project_id",
                "story_no",
                "run_id",
                "lane_index",
                "token",
                "record_json",
            ][..],
        ),
        (
            44,
            "schema_lineage",
            &["source", "source_version", "migrations_json", "bridged_at"][..],
        ),
    ] {
        expect_table(conn, table, columns, !main && version >= minimum)?;
    }
    if version >= 37 {
        let adopted: Option<String> = conn.query_row("SELECT name FROM pragma_table_info('engine_lanes') WHERE name='adopted_identity_json'",[],|row|row.get(0)).optional()?;
        if adopted.is_some() != (!main && version >= 40) {
            return Err(StoreError::Corrupt(
                "schema lineage disagrees with engine adoption capability".into(),
            ));
        }
    }
    Ok(main)
}

/// Upgrades the divergent tail as one transaction, including the lineage decision.
/// The caller has verified a backup; no intermediate collided version is published.
pub(super) fn upgrade(
    conn: &Connection,
    backup_dir: &std::path::Path,
    backup_required: bool,
) -> Result<(Vec<String>, Option<std::path::PathBuf>), StoreError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<(Vec<String>, Option<std::path::PathBuf>), StoreError> {
        let main = classify(conn)?;
        let source_version = schema_version(conn)?;
        if source_version >= 44 {
            return Ok((Vec::new(), None));
        }
        if source_version < 37 {
            return Err(StoreError::Corrupt(
                "launch upgrade requires the shared schema through version 37".into(),
            ));
        }
        let snapshot = if backup_required || main {
            // VACUUM INTO cannot run on the transaction connection. A second
            // connection reads its committed state while this write lock holds
            // all other writers, matching Store::write_with_snapshot.
            let path: String = conn.query_row(
                "SELECT file FROM pragma_database_list WHERE name='main'",
                [],
                |row| row.get(0),
            )?;
            let reader =
                Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            Some(super::snapshot_coupled(
                &reader,
                backup_dir,
                &format!("v{source_version}-lineage"),
            )?)
        } else {
            None
        };
        let original = serde_json::to_string(&history(conn)?)?;
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut applied = Vec::new();
        if main {
            if source_version == 39 {
                // ALTER preserves the original reservation bytes and all constraints.
                conn.execute_batch("ALTER TABLE story_resets RENAME TO story_reset_reservations")?;
            }
            for migration in &MIGRATIONS[37..43] {
                crate::store::fault::fire(crate::store::FaultPoint::MidMigration)?;
                conn.execute_batch(migration.sql)?;
            }
            // Each retained table was checked against its supported full definition.
            let common = MIGRATIONS[43]
                .sql
                .replace(
                    "CREATE TABLE landing_intents",
                    "CREATE TABLE IF NOT EXISTS landing_intents",
                )
                .replace(
                    "CREATE TABLE story_reset_reservations",
                    "CREATE TABLE IF NOT EXISTS story_reset_reservations",
                );
            conn.execute_batch(&common)?;
            conn.execute("INSERT INTO schema_lineage(source,source_version,migrations_json,bridged_at) VALUES ('main',?1,?2,?3)",rusqlite::params![source_version, original, now])?;
            conn.execute("DELETE FROM schema_migrations WHERE version >= 38", [])?;
            for migration in &MIGRATIONS[37..44] {
                conn.execute(
                    "INSERT INTO schema_migrations(version,name,applied_at) VALUES (?1,?2,?3)",
                    rusqlite::params![migration.version, migration.name, now],
                )?;
                applied.push(migration.name.to_string());
            }
        } else {
            for migration in &MIGRATIONS[source_version as usize..44] {
                crate::store::fault::fire(crate::store::FaultPoint::MidMigration)?;
                conn.execute_batch(migration.sql)?;
                conn.execute(
                    "INSERT INTO schema_migrations(version,name,applied_at) VALUES (?1,?2,?3)",
                    rusqlite::params![migration.version, migration.name, now],
                )?;
                applied.push(migration.name.to_string());
            }
        }
        conn.execute_batch("PRAGMA user_version=44")?;
        classify(conn)?;
        let violation: Option<String> = conn
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_check LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(table) = violation {
            return Err(StoreError::Corrupt(format!(
                "schema bridge found a dangling foreign key in {table}"
            )));
        }
        Ok((applied, snapshot))
    })();
    match result {
        Ok(applied) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            Ok(applied)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn validate_definition(conn: &Connection, table: &str) -> Result<(), StoreError> {
    let source = match table {
        "dropped_cleanups" => include_str!("../schema/0045_dropped_cleanup.sql"),
        "landing_intents" => include_str!("../schema/0038_landing_intents.sql"),
        "story_reset_reservations" => include_str!("../schema/0044_launch_compatibility.sql"),
        "story_resets" => {
            let native: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('story_resets') WHERE name='reservation')",[],|row|row.get(0))?;
            if native {
                include_str!("../schema/0039_story_reset.sql")
            } else {
                include_str!("../schema/0043_story_resets.sql")
            }
        }
        "block_deliveries" => include_str!("../schema/0038_block_deliveries.sql"),
        "verification_recovery" => include_str!("../schema/0039_verification_recovery.sql"),
        "continuations" => include_str!("../schema/0041_continuations.sql"),
        "engine_resets" => include_str!("../schema/0042_engine_resets.sql"),
        "schema_lineage" => include_str!("../schema/0044_launch_compatibility.sql"),
        _ => {
            return Err(StoreError::Invariant(format!(
                "no supported schema definition for {table}"
            )));
        }
    };
    let source = source
        .lines()
        .map(|line| line.split("--").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let marker = format!("CREATE TABLE {table} (");
    let start = source.find(&marker).ok_or_else(|| {
        StoreError::Invariant(format!("missing expected CREATE TABLE for {table}"))
    })?;
    let expected = source[start..].split(';').next().expect("table definition");
    let actual: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get(0),
    )?;
    if normalized_definition(expected) != normalized_definition(&actual) {
        return Err(StoreError::Corrupt(format!(
            "schema table `{table}` does not match its supported keys, constraints, and columns"
        )));
    }
    Ok(())
}

fn normalized_definition(sql: &str) -> String {
    sql.chars()
        .filter(|c| !c.is_whitespace() && *c != '"')
        .collect::<String>()
        .to_ascii_lowercase()
}
