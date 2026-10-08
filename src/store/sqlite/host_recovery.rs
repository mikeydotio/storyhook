use crate::store::{HostRecovery, HostRecoveryPending, ProjectId, StoreError};
use rusqlite::{Connection, params};
pub(super) fn list(conn: &Connection) -> Result<Vec<HostRecovery>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "host_recoveries",
        &["id", "fault_key", "revision", "active", "state"],
    )? {
        return Ok(Vec::new());
    }
    let mut statement = conn
        .prepare("SELECT id,fault_key,revision,active,state FROM host_recoveries ORDER BY rowid")
        .map_err(|e| StoreError::from_sqlite(e, "reading host recoveries"))?;
    let rows = statement
        .query_map([], |row| {
            let raw: String = row.get(4)?;
            let state = serde_json::from_str(&raw).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(HostRecovery {
                id: row.get(0)?,
                fault_key: row.get(1)?,
                revision: row.get(2)?,
                active: row.get(3)?,
                state,
            })
        })
        .map_err(|e| StoreError::from_sqlite(e, "querying host recoveries"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| StoreError::from_sqlite(e, "decoding host recoveries"))
}
pub(super) fn insert(conn: &Connection, record: &HostRecovery) -> Result<bool, StoreError> {
    if record.revision != 0 || !record.active || record.id.is_empty() || !record.state.is_object() {
        return Err(StoreError::Validation(
            "new host owner needs active revision-zero identity and state".into(),
        ));
    }
    Ok(conn.execute("INSERT INTO host_recoveries(id,fault_key,revision,active,state) VALUES(?1,?2,0,1,?3) ON CONFLICT(fault_key) DO NOTHING",params![record.id,record.fault_key,record.state.to_string()]).map_err(|e|StoreError::from_sqlite(e,"enrolling host recovery"))?==1)
}
pub(super) fn update(
    conn: &Connection,
    record: &HostRecovery,
    expected: i64,
) -> Result<bool, StoreError> {
    if expected < 0 || expected.checked_add(1) != Some(record.revision) || !record.state.is_object()
    {
        return Err(StoreError::Validation(
            "host owner revision must advance exactly once".into(),
        ));
    }
    Ok(conn.execute("UPDATE host_recoveries SET revision=?1,active=?2,state=?3 WHERE id=?4 AND fault_key=?5 AND revision=?6 AND (active=1 OR ?2=0)",params![record.revision,record.active,record.state.to_string(),record.id,record.fault_key,expected]).map_err(|e|StoreError::from_sqlite(e,"updating host recovery"))?==1)
}

pub(super) fn pending(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<HostRecoveryPending>, StoreError> {
    if !crate::store::migrate::has_columns(
        conn,
        "host_recovery_pending",
        &["id", "project_id", "story_no", "generation", "evidence"],
    )? {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare("SELECT id,story_no,generation,evidence FROM host_recovery_pending WHERE project_id=?1 ORDER BY rowid").map_err(|e| StoreError::from_sqlite(e,"reading pending host custody"))?;
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
            Ok(HostRecoveryPending {
                id: row.get(0)?,
                project,
                story: row.get(1)?,
                generation: row.get(2)?,
                evidence,
            })
        })
        .map_err(|e| StoreError::from_sqlite(e, "querying pending host custody"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| StoreError::from_sqlite(e, "decoding pending host custody"))
}
pub(super) fn insert_pending(
    conn: &Connection,
    record: &HostRecoveryPending,
) -> Result<(), StoreError> {
    if record.id.is_empty() || !record.evidence.is_object() {
        return Err(StoreError::Validation(
            "pending host custody lacks identity/evidence".into(),
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
                "pending host custody cannot be replaced".into(),
            ))
        };
    }
    conn.execute("INSERT INTO host_recovery_pending(id,project_id,story_no,generation,evidence) VALUES(?1,?2,?3,?4,?5)",params![record.id,record.project.get(),record.story,record.generation,record.evidence.to_string()]).map_err(|e|StoreError::from_sqlite(e,"retaining pending host custody"))?;
    Ok(())
}
