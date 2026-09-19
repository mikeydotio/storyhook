//! Transactional fault identity, revision guards, and immutable attempt evidence.

use crate::store::{
    GlobalSeq, ProjectId, ProjectRecovery, ProjectRecoveryObservation, StoreError, StoryNo,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

fn json(row: &Row<'_>, index: usize) -> rusqlite::Result<serde_json::Value> {
    let text: String = row.get(index)?;
    serde_json::from_str(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

pub(super) fn list(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<ProjectRecovery>, StoreError> {
    let mut stmt = conn.prepare("SELECT id,code,locus,revision,active,state FROM project_recoveries WHERE project_id=?1 ORDER BY rowid")
        .map_err(|error| StoreError::from_sqlite(error, "reading project recoveries"))?;
    let rows = stmt
        .query_map([project.get()], |row| {
            Ok(ProjectRecovery {
                id: row.get(0)?,
                project,
                code: row.get(1)?,
                locus: row.get(2)?,
                revision: row.get(3)?,
                active: row.get(4)?,
                state: json(row, 5)?,
            })
        })
        .map_err(|error| StoreError::from_sqlite(error, "reading project recoveries"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::from_sqlite(error, "decoding project recoveries"))
}

fn observation(row: &Row<'_>) -> rusqlite::Result<ProjectRecoveryObservation> {
    Ok(ProjectRecoveryObservation {
        project: ProjectId::new(row.get(0)?),
        recovery_id: row.get(1)?,
        story: StoryNo::new(row.get(2)?),
        generation: GlobalSeq::new(row.get(3)?),
        attempt_id: row.get(4)?,
        observed_at: row.get(5)?,
        evidence: json(row, 6)?,
    })
}

pub(super) fn observations(
    conn: &Connection,
    project: ProjectId,
    recovery: &str,
) -> Result<Vec<ProjectRecoveryObservation>, StoreError> {
    let mut stmt = conn.prepare("SELECT project_id,recovery_id,story_no,generation,attempt_id,observed_at,evidence FROM project_recovery_observations WHERE project_id=?1 AND recovery_id=?2 ORDER BY rowid")
        .map_err(|error| StoreError::from_sqlite(error, "reading project recovery observations"))?;
    let rows = stmt
        .query_map(params![project.get(), recovery], observation)
        .map_err(|error| StoreError::from_sqlite(error, "reading project recovery observations"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::from_sqlite(error, "decoding project recovery observations"))
}

pub(super) fn insert(conn: &Connection, record: &ProjectRecovery) -> Result<bool, StoreError> {
    if record.revision != 0 || !record.active {
        return Err(StoreError::Validation(
            "new project recovery must be active at revision zero".into(),
        ));
    }
    Ok(conn.execute("INSERT INTO project_recoveries(id,project_id,code,locus,revision,active,state) VALUES(?1,?2,?3,?4,0,1,?5) ON CONFLICT(project_id,code,locus) WHERE active=1 DO NOTHING",
        params![record.id,record.project.get(),record.code,record.locus,record.state.to_string()])
        .map_err(|error| StoreError::from_sqlite(error, "acquiring project fault identity"))? == 1)
}

pub(super) fn update(
    conn: &Connection,
    record: &ProjectRecovery,
    expected: i64,
) -> Result<bool, StoreError> {
    if expected < 0 || expected.checked_add(1) != Some(record.revision) {
        return Err(StoreError::Validation(
            "project recovery revision must advance exactly once".into(),
        ));
    }
    Ok(conn.execute("UPDATE project_recoveries SET revision=?1,active=?2,state=?3 WHERE id=?4 AND project_id=?5 AND code=?6 AND locus=?7 AND revision=?8",
        params![record.revision,record.active,record.state.to_string(),record.id,record.project.get(),record.code,record.locus,expected])
        .map_err(|error| StoreError::from_sqlite(error, "updating project recovery"))? == 1)
}

pub(super) fn insert_observation(
    conn: &Connection,
    record: &ProjectRecoveryObservation,
) -> Result<bool, StoreError> {
    let inserted = conn.execute("INSERT INTO project_recovery_observations(project_id,recovery_id,story_no,generation,attempt_id,observed_at,evidence) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(project_id,attempt_id) DO NOTHING",
        params![record.project.get(),record.recovery_id,record.story.get(),record.generation.get(),record.attempt_id,record.observed_at,record.evidence.to_string()])
        .map_err(|error| StoreError::from_sqlite(error, "appending project recovery observation"))? == 1;
    if inserted {
        return Ok(true);
    }
    let existing = conn.query_row("SELECT project_id,recovery_id,story_no,generation,attempt_id,observed_at,evidence FROM project_recovery_observations WHERE project_id=?1 AND attempt_id=?2",
        params![record.project.get(),record.attempt_id], observation).optional()
        .map_err(|error| StoreError::from_sqlite(error, "checking project observation replay"))?;
    if existing.as_ref() != Some(record) {
        return Err(StoreError::Validation(format!(
            "project recovery attempt {} has conflicting immutable evidence",
            record.attempt_id
        )));
    }
    Ok(false)
}
