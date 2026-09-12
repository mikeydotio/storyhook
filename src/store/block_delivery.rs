//! Durable operational delivery records; story history remains the source of blocks.

use super::{ProjectId, StoryNo};
use serde::{Deserialize, Serialize};

/// The terminal operation requested by an effective block transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockAction {
    /// End the active turn without sending text.
    Interrupt,
    /// Submit the fixed reread prompt to the interrupted session.
    Resume,
}

impl BlockAction {
    /// The stable persistence spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Resume => "resume",
        }
    }
}

/// Delivery acknowledgement, including the unavoidable external-side-effect gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// No external operation has begun.
    Pending,
    /// A delivery may have started; restart must not replay it.
    Attempting,
    /// The helper acknowledged its operation.
    Delivered,
    /// No verified agent could be reached.
    Unreached,
    /// The operation may have reached the terminal, but cannot be proved.
    Uncertain,
    /// A later state made this prompt inapplicable.
    Superseded,
}

impl DeliveryStatus {
    /// The stable persistence spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Attempting => "attempting",
            Self::Delivered => "delivered",
            Self::Unreached => "unreached",
            Self::Uncertain => "uncertain",
            Self::Superseded => "superseded",
        }
    }
}

/// One ordered intent, committed atomically with its effective transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDelivery {
    /// Monotonically ordered row identity.
    pub id: i64,
    /// Project owning this delivery.
    pub project: ProjectId,
    /// Story number within the project.
    pub story: StoryNo,
    /// Requested terminal operation.
    pub action: BlockAction,
    /// Current acknowledgement state.
    pub status: DeliveryStatus,
    /// Exact interrupted session identity, supplied by the verified helper.
    pub target: Option<String>,
    /// Durable explanation of the outcome.
    pub detail: String,
}
