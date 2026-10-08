//! The workspace locks of a batch's members (SH-831; spec B11).
//!
//! The head's lock is taken when its attempt is admitted, before any batch
//! exists. Every other member's lock is tried here in ascending story number,
//! and a lock someone else holds excludes that member instead of waiting.
//! Every workspace-lock acquisition in the system is non-blocking, so no party
//! ever waits while it holds one: two batches, or a batch and a manual action,
//! cannot deadlock whatever order they take locks in.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::AppError;
use crate::service::workspace_lock::WorkspaceLock;
use crate::store::StoryNo;

/// The locks a batch holds for its non-head members, released when this
/// value (or one member's entry) is dropped.
pub(super) struct MemberLocks {
    held: BTreeMap<String, WorkspaceLock>,
}

impl MemberLocks {
    /// Tries each member's lock in ascending story number, never waiting.
    /// Answers the locks it took and, in the same order, the members whose
    /// lock another operation holds.
    #[cfg(test)]
    pub(super) fn acquire(
        checkout: &Path,
        members: &[(StoryNo, String)],
    ) -> Result<(Self, Vec<String>), AppError> {
        Self::acquire_with_bound(
            crate::testing::load_grace::graced_now(std::time::Duration::from_secs(30)),
            checkout,
            members,
        )
    }

    pub(super) fn acquire_with_bound(
        bound: std::time::Duration,
        checkout: &Path,
        members: &[(StoryNo, String)],
    ) -> Result<(Self, Vec<String>), AppError> {
        let mut ordered: Vec<&(StoryNo, String)> = members.iter().collect();
        ordered.sort_by_key(|(story, _)| *story);
        let mut held = BTreeMap::new();
        let mut busy = Vec::new();
        for (_, story_id) in ordered {
            match WorkspaceLock::try_acquire_with_bound(bound, checkout, story_id)? {
                Some(lock) => {
                    held.insert(story_id.clone(), lock);
                }
                None => busy.push(story_id.clone()),
            }
        }
        Ok((Self { held }, busy))
    }

    /// The lock this batch holds for `story_id`.
    pub(super) fn get(&self, story_id: &str) -> Option<&WorkspaceLock> {
        self.held.get(story_id)
    }

    /// Releases one member's lock: the member left the batch.
    pub(super) fn release(&mut self, story_id: &str) {
        self.held.remove(story_id);
    }
}
