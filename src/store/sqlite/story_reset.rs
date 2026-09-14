//! Transactional storage for standalone reset reservations.
use crate::store::{ProjectId, StoreError, StoryNo, StoryReset};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn read(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<StoryReset>, StoreError> {
    let json: Option<String> = conn
        .query_row(
            "SELECT record_json FROM story_resets WHERE project_id=?1 AND story_no=?2",
            params![project.get(), story.get()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StoreError::from_sqlite(e, "reading story reset"))?;
    json.map(|json| {
        serde_json::from_str(&json)
            .map_err(|e| StoreError::Corrupt(format!("invalid story reset: {e}")))
    })
    .transpose()
}

pub(super) fn put(conn: &Connection, reset: &StoryReset) -> Result<(), StoreError> {
    if let Some(current) = read(conn, reset.project, reset.story)?
        && !current.completed
    {
        if current.token != reset.token
            || current.story_id != reset.story_id
            || current.original_state != reset.original_state
            || current.lanes != reset.lanes
            || (current.resources.is_some()
                && serde_json::to_value(&current.resources).ok()
                    != serde_json::to_value(&reset.resources).ok())
        {
            return Err(StoreError::Invariant("story reset identity changed".into()));
        }
    }
    let json = serde_json::to_string(reset)
        .map_err(|e| StoreError::Invariant(format!("encoding story reset: {e}")))?;
    let changed = conn.execute("INSERT INTO story_resets(project_id,story_no,token,record_json) VALUES (?1,?2,?3,?4) ON CONFLICT(project_id,story_no) DO UPDATE SET token=excluded.token,record_json=excluded.record_json WHERE story_resets.token=excluded.token OR json_extract(story_resets.record_json,'$.completed')=1", params![reset.project.get(),reset.story.get(),reset.token,json]).map_err(|e| StoreError::from_sqlite(e,"reserving story reset"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(StoreError::Invariant(
            "story reset ownership changed".into(),
        ))
    }
}
