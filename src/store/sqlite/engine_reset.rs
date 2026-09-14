//! SQL for persistent lane-reset ownership.

use crate::store::{EngineReset, ProjectId, StoreError, StoryNo};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn read(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<EngineReset>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "engine_resets",
        &["project_id", "story_no", "token", "record_json"],
    )? {
        return Ok(None);
    }
    let json: Option<String> = conn
        .query_row(
            "SELECT record_json FROM engine_resets WHERE project_id=?1 AND story_no=?2",
            params![project.get(), story.get()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StoreError::from_sqlite(e, "reading engine reset ownership"))?;
    json.map(|json| {
        serde_json::from_str(&json)
            .map_err(|e| StoreError::Corrupt(format!("invalid engine reset record: {e}")))
    })
    .transpose()
}

pub(super) fn put(conn: &Connection, reset: &EngineReset) -> Result<(), StoreError> {
    let json = serde_json::to_string(reset)
        .map_err(|e| StoreError::Invariant(format!("encoding engine reset: {e}")))?;
    conn.execute(
        "INSERT INTO engine_resets (project_id,story_no,run_id,lane_index,token,record_json) VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(project_id,story_no) DO UPDATE SET record_json=excluded.record_json WHERE engine_resets.token=excluded.token AND json_remove(engine_resets.record_json, '$.failure')=json_remove(excluded.record_json, '$.failure')",
        params![reset.project.get(), reset.story.get(), reset.run_id, reset.lane_index, reset.token, json],
    ).map_err(|e| StoreError::from_sqlite(e, "reserving an engine reset")).and_then(|changed| {
        if changed == 1 { Ok(()) } else { Err(StoreError::Invariant("engine reset ownership changed".into())) }
    })
}

pub(super) fn remove(conn: &Connection, reset: &EngineReset) -> Result<(), StoreError> {
    let changed = conn
        .execute(
            "DELETE FROM engine_resets WHERE project_id=?1 AND story_no=?2 AND token=?3",
            params![reset.project.get(), reset.story.get(), reset.token],
        )
        .map_err(|e| StoreError::from_sqlite(e, "finishing an engine reset"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(StoreError::Invariant(
            "engine reset ownership changed before completion".into(),
        ))
    }
}
