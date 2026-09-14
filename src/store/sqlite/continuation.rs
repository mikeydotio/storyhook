//! SQLite compare-and-swap persistence for continuation receipts.
use crate::store::{Continuation, ProjectId, StoreError};
use rusqlite::{Connection, params};
pub(super) fn list(conn: &Connection, project: ProjectId) -> Result<Vec<Continuation>, StoreError> {
    let mut stmt = conn
        .prepare("SELECT record FROM continuations WHERE project_id=?1 ORDER BY rowid")
        .map_err(|e| StoreError::from_sqlite(e, "reading continuations"))?;
    let rows = stmt
        .query_map([project.get()], |r| r.get::<_, String>(0))
        .map_err(|e| StoreError::from_sqlite(e, "reading continuations"))?;
    rows.map(|r| {
        let json = r.map_err(|e| StoreError::from_sqlite(e, "reading continuation"))?;
        serde_json::from_str(&json)
            .map_err(|e| StoreError::Corrupt(format!("continuation record: {e}")))
    })
    .collect()
}
pub(super) fn insert(conn: &Connection, record: &Continuation) -> Result<(), StoreError> {
    let json = serde_json::to_string(record).map_err(|e| StoreError::Validation(e.to_string()))?;
    conn.execute(
        "INSERT INTO continuations(id,project_id,story_no,revision,record) VALUES(?1,?2,?3,?4,?5)",
        params![
            record.id,
            record.project_id.get(),
            record.story_no.get(),
            record.revision,
            json
        ],
    )
    .map_err(|e| StoreError::from_sqlite(e, "creating continuation"))?;
    Ok(())
}
pub(super) fn update(
    conn: &Connection,
    record: &Continuation,
    expected: i64,
) -> Result<bool, StoreError> {
    if record.revision != expected + 1 {
        return Err(StoreError::Validation(
            "continuation revision must advance exactly once".into(),
        ));
    }
    let json = serde_json::to_string(record).map_err(|e| StoreError::Validation(e.to_string()))?;
    Ok(conn.execute("UPDATE continuations SET revision=?1,record=?2 WHERE id=?3 AND project_id=?4 AND story_no=?5 AND revision=?6",params![record.revision,json,record.id,record.project_id.get(),record.story_no.get(),expected]).map_err(|e|StoreError::from_sqlite(e,"updating continuation"))?==1)
}
