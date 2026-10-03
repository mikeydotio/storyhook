//! Persistence for non-authoritative gate cost evidence.
use crate::store::{GateAttempt, ProjectId, StoreError};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn list(conn: &Connection, project: ProjectId) -> Result<Vec<GateAttempt>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "gate_attempts",
        &["id", "project_id", "story_id", "revision", "payload"],
    )? {
        return Ok(vec![]);
    }
    let mut statement = conn
        .prepare("SELECT payload FROM gate_attempts WHERE project_id=?1 ORDER BY rowid")
        .map_err(|e| StoreError::from_sqlite(e, "reading gate cost evidence"))?;
    statement
        .query_map([project.get()], |r| r.get::<_, String>(0))
        .map_err(|e| StoreError::from_sqlite(e, "querying gate cost evidence"))?
        .map(|r| decode(&r.map_err(|e| StoreError::from_sqlite(e, "decoding gate cost row"))?))
        .collect()
}

pub(super) fn insert(conn: &Connection, record: &GateAttempt) -> Result<(), StoreError> {
    record.validate()?;
    if record.revision != 0 || record.finished_at.is_some() {
        return Err(StoreError::Validation(
            "new gate evidence must be live at revision zero".into(),
        ));
    }
    conn.execute("INSERT INTO gate_attempts(id, project_id, story_id, revision, payload) VALUES(?1,?2,?3,0,?4)",
        params![record.id, record.submission.project.get(), record.submission.story_id, encode(record)?])
        .map_err(|e| StoreError::from_sqlite(e, "inserting gate cost evidence"))?;
    Ok(())
}

pub(super) fn update(
    conn: &Connection,
    record: &GateAttempt,
    expected: i64,
) -> Result<bool, StoreError> {
    record.validate()?;
    if expected < 0 || expected.checked_add(1) != Some(record.revision) {
        return Err(StoreError::Validation(
            "gate evidence revision must advance exactly once".into(),
        ));
    }
    let old = conn
        .query_row(
            "SELECT payload FROM gate_attempts WHERE id=?1 AND project_id=?2 AND revision=?3",
            params![record.id, record.submission.project.get(), expected],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| StoreError::from_sqlite(e, "checking gate evidence revision"))?;
    let Some(old) = old else { return Ok(false) };
    let old = decode(&old)?;
    let mut same_revision = record.clone();
    same_revision.revision = old.revision;
    let immutable_changed = (old.finished_at.is_some() && old != same_revision)
        || old.submission != record.submission
        || old.admitted_at != record.admitted_at
        || old.control_revision != record.control_revision
        || old.previous_attempt != record.previous_attempt
        || (old.finished_at.is_some() && old.finished_at != record.finished_at)
        || (old.elapsed.breached_at.is_some()
            && old.elapsed.breached_at != record.elapsed.breached_at)
        || record.elapsed.milliseconds < old.elapsed.milliseconds
        || (old.elapsed.estimated && !record.elapsed.estimated)
        || old
            .diagnostics
            .iter()
            .any(|d| !record.diagnostics.contains(d))
        || record.executions.len() < old.executions.len()
        || old
            .executions
            .iter()
            .zip(&record.executions)
            .any(|(old, next)| !old.preserved_by(next))
        || !crate::store::gate_evidence::intervals_preserved(&old.intervals, &record.intervals);
    if immutable_changed {
        return Err(StoreError::Validation(format!(
            "gate evidence {} cannot replace immutable identity, completion or elapsed history",
            record.id
        )));
    }
    Ok(conn
        .execute(
            "UPDATE gate_attempts SET revision=?1,payload=?2 WHERE id=?3 AND revision=?4",
            params![record.revision, encode(record)?, record.id, expected],
        )
        .map_err(|e| StoreError::from_sqlite(e, "updating gate cost evidence"))?
        == 1)
}

fn decode(payload: &str) -> Result<GateAttempt, StoreError> {
    let record: GateAttempt = serde_json::from_str(payload)
        .map_err(|e| StoreError::Corrupt(format!("invalid gate evidence: {e}")))?;
    record
        .validate()
        .map_err(|e| StoreError::Corrupt(format!("invalid gate evidence {}: {e}", record.id)))?;
    Ok(record)
}

fn encode(record: &GateAttempt) -> Result<String, StoreError> {
    serde_json::to_string(record)
        .map_err(|e| StoreError::Invariant(format!("encoding gate evidence: {e}")))
}
