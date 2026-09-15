//! Reservations share the store's transaction and resource-ownership fence.
use crate::store::{DroppedCleanup, ProjectId, StoreError, StoryNo};
use rusqlite::{Connection, OptionalExtension, params};

/// Reads a typed reservation without mistaking an older schema for corruption.
pub(super) fn read(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<DroppedCleanup>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "dropped_cleanups",
        &["project_id", "story_no", "token", "record_json"],
    )? {
        return Ok(None);
    }
    let stored: Option<(String, String)> = conn
        .query_row(
            "SELECT token,record_json FROM dropped_cleanups WHERE project_id=?1 AND story_no=?2",
            params![project.get(), story.get()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    stored
        .map(|(token, json)| {
            let record: DroppedCleanup = serde_json::from_str(&json)
                .map_err(|e| StoreError::Corrupt(format!("invalid dropped cleanup: {e}")))?;
            if record.project != project || record.story != story || record.token != token {
                return Err(StoreError::Corrupt(
                    "dropped cleanup identity disagrees with its key".into(),
                ));
            }
            Ok(record)
        })
        .transpose()
}

/// Preserves immutable ownership and admits only forward cleanup progress.
pub(super) fn put(conn: &Connection, record: &DroppedCleanup) -> Result<(), StoreError> {
    use crate::store::DroppedCleanupPhase as Phase;
    if record.token.is_empty()
        || !record
            .token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(StoreError::Invariant(
            "invalid dropped cleanup token".into(),
        ));
    }
    if record.released && matches!(record.phase, Phase::Stopping | Phase::Removing) {
        return Err(StoreError::Invariant(
            "unsettled dropped cleanup cannot release ownership".into(),
        ));
    }
    if let Some(current) = read(conn, record.project, record.story)? {
        if current.token != record.token && !current.released {
            return Err(StoreError::Invariant(
                "dropped cleanup still owns this story".into(),
            ));
        }
        if current.token == record.token
            && !matches!(
                (current.phase, record.phase),
                (Phase::Prepared, Phase::Prepared | Phase::Stopping)
                    | (Phase::Stopping, Phase::Stopping | Phase::Quiescent)
                    | (
                        Phase::Quiescent,
                        Phase::Quiescent | Phase::Removing | Phase::Removed
                    )
                    | (Phase::Removing, Phase::Removing | Phase::Removed)
                    | (Phase::Removed, Phase::Removed)
            )
        {
            return Err(StoreError::Invariant(
                "dropped cleanup progress cannot move backwards or skip effects".into(),
            ));
        }
        if current.token == record.token
            && (current.generation != record.generation
                || current.lease != record.lease
                || current.paths != record.paths
                || current.process_start != record.process_start
                || serde_json::to_value(&current.resources)?
                    != serde_json::to_value(&record.resources)?)
        {
            return Err(StoreError::Invariant(
                "dropped cleanup resource identity changed".into(),
            ));
        }
    }
    let json = serde_json::to_string(record)?;
    conn.execute("INSERT INTO dropped_cleanups(project_id,story_no,token,record_json) VALUES (?1,?2,?3,?4) ON CONFLICT(project_id,story_no) DO UPDATE SET token=excluded.token,record_json=excluded.record_json", params![record.project.get(), record.story.get(), record.token, json])?;
    Ok(())
}

/// Rejects lifecycle writes while dropped cleanup retains unsettled effects.
pub(super) fn refuse(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<(), StoreError> {
    if let Some(record) = read(conn, project, story)?
        && !record.released
    {
        return Err(StoreError::Invariant(format!(
            "dropped cleanup {} owns {}; retry story cleanup before changing it",
            record.token, record.lease.story_id
        )));
    }
    Ok(())
}
