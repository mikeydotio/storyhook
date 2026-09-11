//! Immutable evidence supplied by verification before a merge is authorized.

use serde::{Deserialize, Serialize};

/// The exact Git objects and gate certified by a verification attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedSubmission {
    /// Pull request head that was tested.
    pub head: String,
    /// Certified proposed merge tree.
    pub tree: String,
    /// Human-readable gate command used for the certification.
    pub gate: String,
}

impl VerifiedSubmission {
    /// Rejects incomplete certification before it can authorize a process argument.
    pub fn validate(&self) -> Result<(), crate::error::AppError> {
        for (name, oid) in [("head", &self.head), ("tree", &self.tree)] {
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(crate::error::AppError::Validation(format!(
                    "verified {name} must be a full Git object id"
                )));
            }
        }
        if self.gate.trim().is_empty() {
            return Err(crate::error::AppError::Validation(
                "verification certification has no gate".into(),
            ));
        }
        Ok(())
    }
}
