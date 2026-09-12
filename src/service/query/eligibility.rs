//! One transaction owns the tracker facts behind autonomous continuation.

use serde::Serialize;

use super::QueryService;
use crate::domain::{SuperState, is_ready};
use crate::error::AppError;
use crate::store::ReadOps;

/// Stable explanation for whether a previously claimed session may continue.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EligibilityReason {
    /// The story is active, open, and ready under the domain rules.
    Eligible,
    /// The story is closed.
    Closed,
    /// The story is not in the configured active state.
    Inactive,
    /// An explicit awaiting reason requires resolution.
    Awaiting,
    /// The domain readiness predicate reports an unmet blocker.
    Blocked,
}

/// A read-only tracker snapshot, not provider identity or operational approval.
#[derive(Debug, Serialize)]
pub struct SessionEligibility {
    /// Version of this wire contract.
    pub schema_version: u8,
    /// Canonical project-scoped story ID.
    pub story_id: String,
    /// Whether the tracker permits the existing session to continue.
    pub eligible: bool,
    /// Why the tracker returned this answer.
    pub reason: EligibilityReason,
}

impl<R: ReadOps> QueryService<'_, R> {
    /// Read eligibility within this query's transaction, without mutating state.
    pub fn session_eligibility(&self, id: &str) -> Result<SessionEligibility, AppError> {
        let stories = self.story_map()?;
        let story = stories
            .get(id)
            .ok_or_else(|| AppError::NotFound(format!("story `{id}` not found")))?;
        let active = self.active_state()?.ok_or_else(|| {
            AppError::Usage(
                "session eligibility: project has no unambiguous active state role".into(),
            )
        })?;
        let reason = if story.superstate != SuperState::Open {
            EligibilityReason::Closed
        } else if story.state != active.slug {
            EligibilityReason::Inactive
        } else if story.awaiting.is_some() {
            EligibilityReason::Awaiting
        } else if !is_ready(story, &stories) {
            EligibilityReason::Blocked
        } else {
            EligibilityReason::Eligible
        };
        Ok(SessionEligibility {
            schema_version: 1,
            story_id: story.id.clone(),
            eligible: reason == EligibilityReason::Eligible,
            reason,
        })
    }
}
