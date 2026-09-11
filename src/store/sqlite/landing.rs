//! SQLite persistence for immutable landing authority.

use crate::store::{LandingIntent, StoreError};
use rusqlite::{Connection, params};

pub(super) fn read(conn: &Connection) -> Result<Vec<LandingIntent>, StoreError> {
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|e| StoreError::from_sqlite(e, "reading landing schema version"))?;
    // Old schema fixtures and read-only historical stores have no intents.
    if version < 38 {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT payload FROM landing_intents ORDER BY project_id, story_no")
        .map_err(|e| StoreError::from_sqlite(e, "reading landing intents"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| StoreError::from_sqlite(e, "querying landing intents"))?;
    rows.map(|row| {
        let payload = row.map_err(|e| StoreError::from_sqlite(e, "reading a landing intent"))?;
        serde_json::from_str(&payload)
            .map_err(|e| StoreError::Corrupt(format!("invalid landing intent: {e}")))
    })
    .collect()
}

pub(super) fn insert(conn: &Connection, intent: &LandingIntent) -> Result<(), StoreError> {
    let payload = serde_json::to_string(intent)
        .map_err(|e| StoreError::Invariant(format!("encoding landing intent: {e}")))?;
    conn.execute(
        "INSERT INTO landing_intents(id, project_id, story_no, payload) VALUES (?1, ?2, ?3, ?4)",
        params![intent.id, intent.project.get(), intent.story.get(), payload],
    )
    .map_err(|e| StoreError::from_sqlite(e, "admitting landing intent"))?;
    Ok(())
}

pub(super) fn remove(conn: &Connection, intent: &LandingIntent) -> Result<bool, StoreError> {
    let payload = serde_json::to_string(intent)
        .map_err(|e| StoreError::Invariant(format!("encoding landing intent: {e}")))?;
    Ok(conn
        .execute(
            "DELETE FROM landing_intents WHERE id = ?1 AND payload = ?2",
            params![intent.id, payload],
        )
        .map_err(|e| StoreError::from_sqlite(e, "resolving landing intent"))?
        == 1)
}
