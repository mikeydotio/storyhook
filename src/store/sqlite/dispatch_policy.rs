//! Policy rows are updated under the store's write transaction.
use crate::domain::Complexity;
use crate::store::{DispatchPolicyOverride, EngineAgent, ProjectId, StoreError};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn read(
    conn: &Connection,
    project: Option<ProjectId>,
    agent: EngineAgent,
    complexity: Complexity,
) -> Result<DispatchPolicyOverride, StoreError> {
    let decode = |row: &rusqlite::Row<'_>| {
        Ok(DispatchPolicyOverride {
            model: row.get(0)?,
            effort: row.get(1)?,
        })
    };
    let result = match project {
        Some(project) => conn.query_row("SELECT model, effort FROM dispatch_policy_project WHERE project_id = ?1 AND agent = ?2 AND complexity = ?3", params![project.get(), agent.as_str(), complexity.as_str()], decode),
        None => conn.query_row("SELECT model, effort FROM dispatch_policy_global WHERE agent = ?1 AND complexity = ?2", params![agent.as_str(), complexity.as_str()], decode),
    };
    result
        .optional()
        .map(|row| row.unwrap_or_default())
        .map_err(|e| StoreError::from_sqlite(e, "reading dispatch policy"))
}

pub(super) fn write(
    conn: &Connection,
    project: Option<ProjectId>,
    agent: EngineAgent,
    complexity: Complexity,
    value: &DispatchPolicyOverride,
) -> Result<(), StoreError> {
    let result = match project {
        Some(project) => conn.execute("INSERT INTO dispatch_policy_project (project_id, agent, complexity, model, effort) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(project_id, agent, complexity) DO UPDATE SET model = excluded.model, effort = excluded.effort", params![project.get(), agent.as_str(), complexity.as_str(), value.model, value.effort]),
        None => conn.execute("INSERT INTO dispatch_policy_global (agent, complexity, model, effort) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(agent, complexity) DO UPDATE SET model = excluded.model, effort = excluded.effort", params![agent.as_str(), complexity.as_str(), value.model, value.effort]),
    };
    result
        .map(|_| ())
        .map_err(|e| StoreError::from_sqlite(e, "writing dispatch policy"))
}
