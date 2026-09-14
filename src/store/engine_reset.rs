//! Durable ownership of an explicitly requested, unfinished lane reset.

use serde::{Deserialize, Serialize};

use super::{ProjectId, StoryNo};
use crate::domain::StoryCleanupLease;

/// One reset whose resources must disappear before its story becomes ready.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineReset {
    /// Project owning the story, independent of its display prefix.
    pub project: ProjectId,
    /// Story number within that project.
    pub story: StoryNo,
    /// Run that accepted the operator's Stop Now request.
    pub run_id: String,
    /// Lane reserved until cleanup and state restoration finish.
    pub lane_index: u32,
    /// Unique identity echoed by every helper observation and receipt.
    pub token: String,
    /// Creation-time resource identity; never inferred from current settings.
    pub lease: StoryCleanupLease,
    /// Open, non-verification state to restore after successful cleanup.
    pub restore_to: String,
    /// Most recent failed cleanup diagnosis, retained across restarts.
    pub failure: Option<String>,
}
