//! Revision-checked causal evidence, separate from certification.
use crate::service::attribution::AttributionRecord;
use crate::store::{ProjectId, StoreError};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn list(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<AttributionRecord>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "verification_attributions",
        &["id", "project_id", "story_id", "revision", "payload"],
    )? {
        return Ok(vec![]);
    }
    let mut query = conn.prepare(
        "SELECT payload FROM verification_attributions WHERE project_id=?1 ORDER BY rowid",
    )?;
    query
        .query_map([project.get()], |row| row.get::<_, String>(0))?
        .map(|row| decode(&row?))
        .collect()
}

pub(super) fn insert(conn: &Connection, record: &AttributionRecord) -> Result<(), StoreError> {
    record.validate()?;
    if record.revision != 0
        || !record.held
        || !record.probes.is_empty()
        || !record.assessments.is_empty()
        || record.diagnosis_ms != 0
    {
        return Err(StoreError::Validation(
            "new attribution must be an unexecuted hold at revision zero".into(),
        ));
    }
    conn.execute("INSERT INTO verification_attributions(id,project_id,story_id,generation,attempt_id,revision,payload) VALUES(?1,?2,?3,?4,?5,0,?6)",
        params![record.id, record.submission.project.get(), record.submission.story_id, record.submission.generation.map(|g| g.get()), record.attempt, encode(record)?])
        .map_err(|e| StoreError::from_sqlite(e, "inserting attribution hold"))?;
    Ok(())
}

pub(super) fn update(
    conn: &Connection,
    record: &AttributionRecord,
    expected: i64,
) -> Result<bool, StoreError> {
    record.validate()?;
    if expected < 0 || expected.checked_add(1) != Some(record.revision) {
        return Err(StoreError::Validation(
            "attribution revision must advance exactly once".into(),
        ));
    }
    let old = conn.query_row("SELECT payload FROM verification_attributions WHERE id=?1 AND project_id=?2 AND revision=?3",
        params![record.id, record.submission.project.get(), expected], |row| row.get::<_, String>(0)).optional()?;
    let Some(old) = old else {
        return Ok(false);
    };
    let old = decode(&old)?;
    if !old.preserved_by(record) {
        return Err(StoreError::Validation(format!(
            "attribution {} cannot replace immutable identity, observations or consumed allowance",
            record.id
        )));
    }
    check_reservation(conn, &old, record)?;
    Ok(conn.execute("UPDATE verification_attributions SET revision=?1,payload=?2 WHERE id=?3 AND project_id=?4 AND revision=?5",
        params![record.revision, encode(record)?, record.id, record.submission.project.get(), expected])? == 1)
}

fn check_reservation(
    conn: &Connection,
    old: &AttributionRecord,
    next: &AttributionRecord,
) -> Result<(), StoreError> {
    if next.probes.len() == old.probes.len() {
        return Ok(());
    }
    let refused = |why: &str| {
        StoreError::Validation(format!(
            "attribution {} cannot reserve a probe: {why}",
            next.id
        ))
    };
    if next.submission.generation.is_none()
        || next.probes.len() != old.probes.len() + 1
        || next.probes.last().is_none_or(|p| p.completed.is_some())
        || !next.held
        || next.diagnosis_ms != old.diagnosis_ms
        || !next.probes.starts_with(&old.probes)
    {
        return Err(refused(
            "one unfinished reservation with an exact live submission is required",
        ));
    }
    let records = list(conn, next.submission.project)?;
    let mut starts = 0usize;
    let mut milliseconds = 0u64;
    for record in records.iter().filter(|r| {
        r.submission.story_id == next.submission.story_id
            && r.submission.generation == next.submission.generation
    }) {
        starts = starts
            .checked_add(record.probes.len())
            .ok_or_else(|| refused("probe count overflow"))?;
        milliseconds = milliseconds
            .checked_add(record.diagnosis_ms)
            .ok_or_else(|| refused("elapsed time overflow"))?;
        if record.probes.iter().any(|p| p.completed.is_none()) {
            return Err(refused("an earlier reserved execution remains unsettled"));
        }
    }
    if starts >= crate::service::attribution::MAX_PROBES
        || milliseconds >= crate::service::attribution::MAX_DIAGNOSIS_MS
    {
        return Err(refused(
            "this submission exhausted its retained diagnosis allowance",
        ));
    }
    Ok(())
}

fn decode(payload: &str) -> Result<AttributionRecord, StoreError> {
    let record: AttributionRecord = serde_json::from_str(payload)
        .map_err(|e| StoreError::Corrupt(format!("invalid attribution payload: {e}")))?;
    record
        .validate()
        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(record)
}

fn encode(record: &AttributionRecord) -> Result<String, StoreError> {
    serde_json::to_string(record)
        .map_err(|e| StoreError::Invariant(format!("encoding attribution: {e}")))
}
