//! SQLite persistence for verification batch records (SH-831).
//!
//! The whole record is the JSON payload; `revision` and `live` are copies the
//! schema ties to it with CHECKs, so the one-live-batch-per-project index and
//! the compare-and-swap update read columns, never JSON.

use crate::store::{ProjectId, StoreError, VerificationBatch};
use rusqlite::{Connection, params};

pub(super) fn list(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<VerificationBatch>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "verification_batches",
        &["id", "project_id", "revision", "live", "payload"],
    )? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT payload FROM verification_batches WHERE project_id = ?1 ORDER BY rowid")
        .map_err(|e| StoreError::from_sqlite(e, "reading verification batches"))?;
    let rows = stmt
        .query_map([project.get()], |row| row.get::<_, String>(0))
        .map_err(|e| StoreError::from_sqlite(e, "querying verification batches"))?;
    rows.map(|row| {
        let payload =
            row.map_err(|e| StoreError::from_sqlite(e, "reading a verification batch"))?;
        serde_json::from_str(&payload)
            .map_err(|e| StoreError::Corrupt(format!("invalid verification batch: {e}")))
    })
    .collect()
}

pub(super) fn insert(conn: &Connection, batch: &VerificationBatch) -> Result<(), StoreError> {
    batch.validate()?;
    if batch.revision != 0 || !batch.phase.is_live() {
        return Err(StoreError::Validation(format!(
            "verification batch {} must be recorded live at revision zero",
            batch.id
        )));
    }
    conn.execute(
        "INSERT INTO verification_batches(id, project_id, revision, live, payload) VALUES (?1, ?2, 0, 1, ?3)",
        params![batch.id.as_str(), batch.project.get(), encode(batch)?],
    )
    .map_err(|e| StoreError::from_sqlite(e, "recording verification batch"))?;
    Ok(())
}

pub(super) fn update(
    conn: &Connection,
    batch: &VerificationBatch,
    expected: i64,
) -> Result<bool, StoreError> {
    batch.validate()?;
    if expected < 0 || expected.checked_add(1) != Some(batch.revision) {
        return Err(StoreError::Validation(format!(
            "verification batch {} revision must advance exactly once",
            batch.id
        )));
    }
    // An ended row may record its retirement, but keeps its phase: once a
    // batch has ended it never becomes live or ends a second way.
    Ok(conn
        .execute(
            "UPDATE verification_batches SET revision = ?1, live = ?2, payload = ?3
             WHERE id = ?4 AND project_id = ?5 AND revision = ?6
               AND (live = 1 OR json_extract(payload, '$.phase') = ?7)",
            params![
                batch.revision,
                batch.phase.is_live(),
                encode(batch)?,
                batch.id.as_str(),
                batch.project.get(),
                expected,
                batch.phase.as_str(),
            ],
        )
        .map_err(|e| StoreError::from_sqlite(e, "updating verification batch"))?
        == 1)
}

pub(super) fn prune(
    conn: &Connection,
    project: ProjectId,
    keep: usize,
) -> Result<usize, StoreError> {
    let keep = i64::try_from(keep)
        .map_err(|_| StoreError::Validation("batch retention is out of range".into()))?;
    // A batch named by a landing intent (a member a person holds after its
    // batch landed) is never pruned: that intent is validated against it
    // before every commit.
    conn.execute(
        "DELETE FROM verification_batches
         WHERE project_id = ?1 AND live = 0 AND json_extract(payload, '$.retired') = 1
           AND id NOT IN (
             SELECT json_extract(payload, '$.batch.id') FROM landing_intents
             WHERE json_extract(payload, '$.batch.id') IS NOT NULL)
           AND rowid NOT IN (
             SELECT rowid FROM verification_batches
             WHERE project_id = ?1 AND live = 0 AND json_extract(payload, '$.retired') = 1
             ORDER BY rowid DESC LIMIT ?2)",
        params![project.get(), keep],
    )
    .map_err(|e| StoreError::from_sqlite(e, "pruning verification batches"))
}

fn encode(batch: &VerificationBatch) -> Result<String, StoreError> {
    serde_json::to_string(batch)
        .map_err(|e| StoreError::Invariant(format!("encoding verification batch: {e}")))
}
