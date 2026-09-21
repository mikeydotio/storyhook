//! Completed wire evidence is checked against admission before spending a repair slot.

use super::{RepairCompletion, RepairInput};
use crate::{service::project_fault::ProjectFault, store::StoreError};
use serde::{Deserialize, Serialize};

/// The settled judgment reported by the verifier, without permission to merge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RepairJudgment {
    /// The exact head and proposed tree were certified.
    Certified {
        /// Judged repair head.
        head: String,
        /// Certified proposed merge tree.
        tree: String,
    },
    /// The required gate completed with a nonzero result.
    TestsFailed {
        /// Judged proposed merge tree.
        tree: String,
    },
    /// The repair encountered another structured project fault.
    ProjectFault {
        /// Exact fault evidence, including all pinned source identities.
        fault: ProjectFault,
    },
}

impl RepairJudgment {
    /// The completed budget category, independent of the retained evidence.
    pub fn classification(&self) -> RepairCompletion {
        match self {
            Self::Certified { .. } => RepairCompletion::Certified,
            Self::TestsFailed { .. } => RepairCompletion::TestsFailed,
            Self::ProjectFault { .. } => RepairCompletion::ProjectFault,
        }
    }

    pub(super) fn validate_against(&self, input: &RepairInput) -> Result<(), StoreError> {
        let matches = match self {
            Self::Certified { head, tree } => head == &input.head && tree == &input.tree,
            Self::TestsFailed { tree } => tree == &input.tree,
            Self::ProjectFault { fault } => {
                fault.validate().map_err(|error| {
                    StoreError::Validation(format!("completed repair fault: {error}"))
                })?;
                let (head, head_tree) = fault.source();
                let (base, tree) = fault.proposed_merge();
                head == input.head
                    && head_tree == input.head_tree
                    && base == input.base
                    && tree == input.tree
            }
        };
        if !matches {
            return Err(StoreError::Validation(
                "completed repair judgment differs from its admitted pinned source or merge input"
                    .into(),
            ));
        }
        Ok(())
    }
}
