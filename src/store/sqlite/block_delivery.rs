//! SQLite persistence of ordered block-delivery intents.
use crate::store::{BlockAction, BlockDelivery, DeliveryStatus, ProjectId, StoreError, StoryNo};
use rusqlite::{Connection, params};

pub(super) fn list(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<BlockDelivery>, StoreError> {
    let context = "reading block delivery intents";
    let mut stmt = conn.prepare("SELECT id, story_no, action, status, target, detail FROM block_deliveries WHERE project_id=?1 ORDER BY id")
        .map_err(|e| StoreError::from_sqlite(e, context))?;
    let rows = stmt
        .query_map([project.get()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| StoreError::from_sqlite(e, context))?;
    rows.map(|row| {
        let (id, story, action, status, target, detail) =
            row.map_err(|e| StoreError::from_sqlite(e, context))?;
        let parse = |value: String| serde_json::Value::String(value);
        Ok(BlockDelivery {
            id,
            project,
            story: StoryNo::new(story),
            action: serde_json::from_value(parse(action))
                .map_err(|e| StoreError::Corrupt(format!("block delivery {id} action: {e}")))?,
            status: serde_json::from_value(parse(status))
                .map_err(|e| StoreError::Corrupt(format!("block delivery {id} status: {e}")))?,
            target,
            detail,
        })
    })
    .collect()
}

pub(super) fn enqueue(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    action: BlockAction,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO block_deliveries (project_id,story_no,action) VALUES (?1,?2,?3)",
        params![project.get(), story.get(), action.as_str()],
    )
    .map_err(|e| StoreError::from_sqlite(e, "recording block delivery intent"))?;
    Ok(())
}

pub(super) fn update(
    conn: &Connection,
    delivery: &BlockDelivery,
    expected: DeliveryStatus,
) -> Result<bool, StoreError> {
    Ok(conn.execute("UPDATE block_deliveries SET status=?1,target=?2,detail=?3 WHERE id=?4 AND project_id=?5 AND story_no=?6 AND status=?7",params![delivery.status.as_str(),delivery.target,delivery.detail,delivery.id,delivery.project.get(),delivery.story.get(),expected.as_str()])
        .map_err(|e| StoreError::from_sqlite(e,"acknowledging block delivery"))? == 1)
}
