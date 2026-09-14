//! Transactional storage for standalone reset reservations.
use crate::store::{ProjectId, StoreError, StoryNo, StoryReset};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn read(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<StoryReset>, StoreError> {
    // Historical migration fixtures legitimately predate standalone reservations.
    if !crate::store::migrate::has_columns(
        conn,
        "story_resets",
        &["project_id", "story_no", "token", "record_json"],
    )? {
        return Ok(None);
    }
    let stored: Option<(String, String)> = conn
        .query_row(
            "SELECT token,record_json FROM story_resets WHERE project_id=?1 AND story_no=?2",
            params![project.get(), story.get()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| StoreError::from_sqlite(e, "reading story reset"))?;
    stored
        .map(|(token, json)| {
            let reset: StoryReset = serde_json::from_str(&json)
                .map_err(|e| StoreError::Corrupt(format!("invalid story reset: {e}")))?;
            if reset.project != project || reset.story != story || reset.token != token {
                return Err(StoreError::Corrupt(
                    "story reset identity disagrees with its storage key".into(),
                ));
            }
            Ok(reset)
        })
        .transpose()
}

pub(super) fn put(conn: &Connection, reset: &StoryReset) -> Result<(), StoreError> {
    if let Some(current) = read(conn, reset.project, reset.story)?
        && !current.completed
        && (current.token != reset.token
            || current.story_id != reset.story_id
            || current.original_state != reset.original_state
            || current.lanes != reset.lanes
            || (current.resources.is_some() && current.paths != reset.paths)
            || (current.resources.is_some()
                && serde_json::to_value(&current.resources).ok()
                    != serde_json::to_value(&reset.resources).ok()))
    {
        return Err(StoreError::Invariant("story reset identity changed".into()));
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

/// Prevents project identity transfer while reset owns any of its resources.
pub(super) fn refuse_project(conn: &Connection, project: ProjectId) -> Result<(), StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "story_resets",
        &["project_id", "story_no", "token", "record_json"],
    )? {
        return Ok(());
    }
    let story: Option<String> = conn.query_row(
        "SELECT json_extract(record_json, '$.story_id') FROM story_resets WHERE project_id=?1 AND json_extract(record_json, '$.completed')=0 LIMIT 1",
        [project.get()], |row| row.get(0)
    ).optional().map_err(|e| StoreError::from_sqlite(e, "checking project reset ownership"))?;
    if let Some(story) = story {
        return Err(StoreError::Invariant(format!(
            "reset owns project resources for {story}; finish Reset first"
        )));
    }
    Ok(())
}
