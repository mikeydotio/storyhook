//! Effective transitions are derived once per complete service transaction.
use super::Ctx;
use crate::domain::{SuperState, is_blocked};
use crate::store::{BlockAction, ProjectId, ReadOps, Store, StoreError, StoryNo, WriteOps};
use std::collections::BTreeMap;

/// The exact operator-supplied prompt, never synthesized or paraphrased.
pub const UNBLOCK_PROMPT: &str = "Your story experienced a temporary block, which has been lifted. The environment and dev branch may have changed. Please reread your story, its comments, and its relationships to understand the changes, and adjust your work accordingly. If the change is significant, resetting & rebasing the worktree and restarting the story may be appropriate.";

struct State {
    blocked: bool,
    active: bool,
}

fn snapshot(tx: &impl ReadOps, project: ProjectId) -> Result<BTreeMap<StoryNo, State>, StoreError> {
    let prefix = super::project_prefix(tx, project)?;
    let stories = super::query::story_map(tx, project)?;
    stories
        .iter()
        .map(|(id, story)| {
            Ok((
                StoryNo::parse_id(&prefix, id)?,
                State {
                    blocked: is_blocked(story, &stories),
                    active: story.superstate == SuperState::Open && story.state == "in-progress",
                },
            ))
        })
        .collect()
}

impl<S: Store> Ctx<'_, S> {
    /// Commit a story-affecting mutation and its final effective delivery edges.
    /// No external operation occurs while SQLite owns the write transaction.
    pub(crate) fn write_stories<T>(
        &self,
        f: impl FnOnce(&mut S::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.store().write(|tx| {
            let before = snapshot(tx, self.project())?;
            let result = f(tx)?;
            let after = snapshot(tx, self.project())?;
            for (story, next) in after {
                // Newly imported/created rows have no dispatched turn to interrupt.
                let Some(previous) = before.get(&story) else {
                    continue;
                };
                let action = if !previous.blocked && next.blocked {
                    Some(BlockAction::Interrupt)
                } else if previous.blocked && !next.blocked && next.active {
                    Some(BlockAction::Resume)
                } else {
                    None
                };
                if let Some(action) = action {
                    tx.enqueue_block_delivery(self.project(), story, action)?;
                }
            }
            Ok(result)
        })
    }
}
