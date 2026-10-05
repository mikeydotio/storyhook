//! A reset supersedes every other owner of its story (SH-886, decision D4).
//!
//! The store admits one owner per story. A Stop Now reset, a pre-upgrade
//! native reservation or a landing intent that never resolves would otherwise
//! refuse the final lever forever, so the reset's own reservation releases
//! each of them through its explicit release operation, in the same
//! transaction, and names what it superseded.
use crate::store::{ProjectId, StoreError, StoryNo, WriteOps};

/// Releases every other owner of `story` and describes each one released.
pub(super) fn supersede_owners(
    tx: &mut impl WriteOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<String>, StoreError> {
    let mut superseded = Vec::new();
    if let Some(engine) = tx.engine_reset(project, story)? {
        tx.remove_engine_reset(&engine)?;
        superseded.push(format!(
            "Stop Now reset {} of run {} lane {}",
            engine.token, engine.run_id, engine.lane_index
        ));
    }
    if let Some(encoded) = tx.story_resets(project)?.remove(&story) {
        let operation = serde_json::from_str::<serde_json::Value>(&encoded)
            .ok()
            .and_then(|value| value["operation"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "of unknown identity".into());
        tx.put_legacy_story_reset(project, story, None)?;
        superseded.push(format!("story reset operation {operation}"));
    }
    for intent in tx
        .landing_intents()?
        .into_iter()
        .filter(|intent| intent.project == project && intent.story == story)
    {
        superseded.push(crate::service::landing::supersede_for_reset(tx, &intent)?);
    }
    Ok(superseded)
}
