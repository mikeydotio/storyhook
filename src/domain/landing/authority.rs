//! A skipped gate is merge authority, never a certification receipt.

use super::VerifiedSubmission;
use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// Test policy captured by an owned submission attempt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationMode {
    /// Run the configured gate before admitting a merge.
    #[default]
    Gated,
    /// The operator stopped verification; prepare and land without tests.
    VerificationSkipped,
}

/// Explicit policy on a skipped landing; unknown modes cannot decode as authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkippedPolicy {
    /// The owning attempt observed stopped verification at admission.
    #[serde(rename = "verification-skipped")]
    VerificationSkipped,
}

/// Exact prepared input and its stopped-mode admission identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedSubmission {
    /// Why this submission may land without a gate.
    pub mode: SkippedPolicy,
    /// Full submitted head object id.
    pub head: String,
    /// Full proposed merge tree object id.
    pub tree: String,
    /// Owning verifier attempt id, assigned before preparation.
    pub attempt: String,
}

impl SkippedSubmission {
    /// Rejects incomplete input before it can authorize a process argument.
    pub fn validate(&self) -> Result<(), AppError> {
        for (name, oid) in [("head", &self.head), ("tree", &self.tree)] {
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(AppError::Validation(format!(
                    "skipped landing {name} must be a full Git object id"
                )));
            }
        }
        if self.attempt.is_empty()
            || !self
                .attempt
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(AppError::Validation(
                "skipped landing requires a valid owning attempt identity".into(),
            ));
        }
        Ok(())
    }
}

/// Immutable reason an exact merge may proceed.
///
/// Certified JSON retains its original bytes: pending-intent removal compares
/// the whole serialized payload, including records written by older binaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LandingAuthority {
    /// A real successful gate certified the merge tree.
    Certified(VerifiedSubmission),
    /// An admitted stopped-mode attempt prepared the tree without testing it.
    Skipped(SkippedSubmission),
    /// A managed integration has its own controller and merge target.
    Integration(IntegrationAuthority),
}

/// Strict envelope: older certified/skipped readers must reject this shape.
/// This persisted evidence does not itself mint a native effect capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationAuthority {
    /// Versioned managed landing binding.
    pub integration: IntegrationLanding,
}

/// Original submission identity and separately certified managed merge target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationLanding {
    /// Supported envelope version, currently one.
    pub version: u8,
    /// Durable integration owner UUID.
    pub owner: String,
    /// Claimed effect ordinal.
    pub epoch: u32,
    /// Distinct managed merge attempt UUID.
    pub attempt: String,
    /// Managed pull request, never the original story link.
    pub pull_request: String,
    /// Immutable original submitted commit.
    pub original_head: String,
    /// Original base parent preserved by assembly.
    pub pinned_base: String,
    /// Current base commit used for certification.
    pub base: String,
    /// Actual central certificate for the managed head and resolution tree.
    pub certification: VerifiedSubmission,
}

impl IntegrationLanding {
    /// Shape validation only; the dedicated controller must prove ownership,
    /// native ancestry, current policy and the actual gate result separately.
    pub fn validate(&self) -> Result<(), AppError> {
        let invalid = |detail: &str| AppError::Validation(format!("integration landing {detail}"));
        if self.version != 1 || self.epoch == 0 {
            return Err(invalid("requires version one and a claimed effect epoch"));
        }
        for identity in [&self.owner, &self.attempt] {
            if uuid::Uuid::parse_str(identity).is_err() {
                return Err(invalid("requires owner and attempt UUIDs"));
            }
        }
        crate::domain::pr_url::parse_pr_url(&self.pull_request)?;
        for oid in [&self.original_head, &self.pinned_base, &self.base] {
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("requires full original and base object ids"));
            }
        }
        self.certification.validate()
    }
}

impl LandingAuthority {
    /// Validates the evidence without promoting skipped work to certification.
    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::Certified(value) => value.validate(),
            Self::Skipped(value) => value.validate(),
            Self::Integration(value) => value.integration.validate(),
        }
    }

    /// Exact submitted head, independent of the gate policy.
    pub fn head(&self) -> &str {
        match self {
            Self::Certified(value) => &value.head,
            Self::Skipped(value) => &value.head,
            Self::Integration(value) => &value.integration.certification.head,
        }
    }

    /// Exact proposed merge tree, independent of the gate policy.
    pub fn tree(&self) -> &str {
        match self {
            Self::Certified(value) => &value.tree,
            Self::Skipped(value) => &value.tree,
            Self::Integration(value) => &value.integration.certification.tree,
        }
    }

    /// Certified evidence, absent for every skipped authority.
    pub fn certified(&self) -> Option<&VerifiedSubmission> {
        match self {
            Self::Certified(value) => Some(value),
            Self::Skipped(_) => None,
            Self::Integration(value) => Some(&value.integration.certification),
        }
    }

    /// Managed binding, requiring its dedicated controller even when certified.
    pub fn integration(&self) -> Option<&IntegrationLanding> {
        match self {
            Self::Integration(value) => Some(&value.integration),
            Self::Certified(_) | Self::Skipped(_) => None,
        }
    }
}

impl From<VerifiedSubmission> for LandingAuthority {
    fn from(value: VerifiedSubmission) -> Self {
        Self::Certified(value)
    }
}
