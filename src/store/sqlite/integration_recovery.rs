//! SQLite identity and revision guards for single integration owners.
use crate::store::{
    GlobalSeq, IntegrationPending, IntegrationRecovery, ProjectId, StoreError, StoryNo,
};
use rusqlite::{Connection, params};

pub(super) fn list(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<IntegrationRecovery>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "integration_recoveries",
        &[
            "id",
            "project_id",
            "story_no",
            "generation",
            "revision",
            "active",
            "state",
        ],
    )? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT id,story_no,generation,revision,active,state FROM integration_recoveries WHERE project_id=?1 ORDER BY rowid")
        .map_err(|e| StoreError::from_sqlite(e, "reading integration owners"))?;
    let rows = stmt
        .query_map([project.get()], |row| {
            let raw: String = row.get(5)?;
            let state = serde_json::from_str(&raw).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(IntegrationRecovery {
                id: row.get(0)?,
                project,
                story: StoryNo::new(row.get(1)?),
                generation: GlobalSeq::new(row.get(2)?),
                revision: row.get(3)?,
                active: row.get(4)?,
                state,
            })
        })
        .map_err(|e| StoreError::from_sqlite(e, "querying integration owners"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| StoreError::from_sqlite(e, "decoding integration owners"))
}

pub(super) fn insert(conn: &Connection, record: &IntegrationRecovery) -> Result<bool, StoreError> {
    if record.revision != 0
        || !record.active
        || record.id.trim().is_empty()
        || !record.state.is_object()
    {
        return Err(StoreError::Validation(
            "integration owner must start active at revision zero with concrete identity and state"
                .into(),
        ));
    }
    Ok(conn.execute("INSERT INTO integration_recoveries(id,project_id,story_no,generation,revision,active,state) VALUES(?1,?2,?3,?4,0,1,?5) ON CONFLICT(project_id,story_no) WHERE active=1 DO NOTHING",
        params![record.id, record.project.get(), record.story.get(), record.generation.get(), record.state.to_string()])
        .map_err(|e| StoreError::from_sqlite(e, "acquiring integration ownership"))? == 1)
}

pub(super) fn update(
    conn: &Connection,
    record: &IntegrationRecovery,
    expected: i64,
) -> Result<bool, StoreError> {
    if expected < 0 || expected.checked_add(1) != Some(record.revision) || !record.state.is_object()
    {
        return Err(StoreError::Validation(
            "integration revision must advance exactly once with concrete state".into(),
        ));
    }
    Ok(conn.execute("UPDATE integration_recoveries SET revision=?1,active=?2,state=?3 WHERE id=?4 AND project_id=?5 AND story_no=?6 AND generation=?7 AND revision=?8 AND (active=1 OR ?2=0)",
        params![record.revision, record.active, record.state.to_string(), record.id, record.project.get(), record.story.get(), record.generation.get(), expected])
        .map_err(|e| StoreError::from_sqlite(e, "updating integration ownership"))? == 1)
}

pub(super) fn pending(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<IntegrationPending>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "integration_pending",
        &["id", "project_id", "story_no", "generation", "evidence"],
    )? {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare("SELECT id,story_no,generation,evidence FROM integration_pending WHERE project_id=?1 ORDER BY rowid").map_err(|e| StoreError::from_sqlite(e,"reading pending integration custody"))?;
    let rows = statement
        .query_map([project.get()], |row| {
            let raw: String = row.get(3)?;
            let evidence = serde_json::from_str(&raw).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(IntegrationPending {
                id: row.get(0)?,
                project,
                story: StoryNo::new(row.get(1)?),
                generation: GlobalSeq::new(row.get(2)?),
                evidence,
            })
        })
        .map_err(|e| StoreError::from_sqlite(e, "querying pending integration custody"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| StoreError::from_sqlite(e, "decoding pending integration custody"))
}
pub(super) fn insert_pending(
    conn: &Connection,
    record: &IntegrationPending,
) -> Result<(), StoreError> {
    if record.id.is_empty() || !record.evidence.is_object() {
        return Err(StoreError::Validation(
            "pending integration custody lacks identity/evidence".into(),
        ));
    }
    if let Some(previous) = pending(conn, record.project)?
        .iter()
        .find(|p| p.id == record.id)
    {
        return if previous == record {
            Ok(())
        } else {
            Err(StoreError::Validation(
                "pending integration custody cannot be replaced".into(),
            ))
        };
    }
    conn.execute("INSERT INTO integration_pending(id,project_id,story_no,generation,evidence) VALUES(?1,?2,?3,?4,?5)",params![record.id,record.project.get(),record.story.get(),record.generation.get(),record.evidence.to_string()]).map_err(|e|StoreError::from_sqlite(e,"retaining pending integration custody"))?;
    Ok(())
}

pub(super) fn readmissions(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<crate::store::IntegrationReadmission>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "integration_readmissions",
        &["id", "project_id", "story_no", "generation", "evidence"],
    )? {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare("SELECT id,story_no,generation,evidence FROM integration_readmissions WHERE project_id=?1 ORDER BY rowid").map_err(|e| StoreError::from_sqlite(e,"reading clean integration readmission"))?;
    let rows = statement
        .query_map([project.get()], |row| {
            let raw: String = row.get(3)?;
            let evidence = serde_json::from_str(&raw).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(crate::store::IntegrationReadmission {
                id: row.get(0)?,
                project,
                story: StoryNo::new(row.get(1)?),
                generation: GlobalSeq::new(row.get(2)?),
                evidence,
            })
        })
        .map_err(|e| StoreError::from_sqlite(e, "querying clean integration readmission"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| StoreError::from_sqlite(e, "decoding clean integration readmission"))
}
pub(super) fn insert_readmission(
    conn: &Connection,
    record: &crate::store::IntegrationReadmission,
) -> Result<(), StoreError> {
    if record.id.is_empty() || !record.evidence.is_object() {
        return Err(StoreError::Validation(
            "clean integration readmission lacks identity/evidence".into(),
        ));
    }
    if let Some(previous) = readmissions(conn, record.project)?
        .iter()
        .find(|p| p.id == record.id)
    {
        return if previous == record {
            Ok(())
        } else {
            Err(StoreError::Validation(
                "clean integration readmission cannot be replaced".into(),
            ))
        };
    }
    conn.execute("INSERT INTO integration_readmissions(id,project_id,story_no,generation,evidence) VALUES(?1,?2,?3,?4,?5)",params![record.id,record.project.get(),record.story.get(),record.generation.get(),record.evidence.to_string()]).map_err(|e|StoreError::from_sqlite(e,"retaining clean integration readmission"))?;
    Ok(())
}
