//! A separate, default-disabled single-submission integration capability.
//!
//! Native inspection grants only a proposed non-code resolution. A durable
//! managed integration owner must still publish a distinct branch/PR, certify
//! its exact tree, and prove landing; the author's submitted head is immutable.
use super::{
    batch_smoothing::{self, Classification, SmoothedFile},
    project::ProjectPointer,
    trial_merge::{BlobSource, PrivateTrialMerger, TrialMerge, TrialMerger, require_pinned},
};
use crate::{
    domain::conflict_smoothing::{STRATEGY, SmoothPolicy},
    error::AppError,
    process::Cancellation,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Instant};
mod assembly;
mod gate_inputs;
mod landed_observation;
pub use gate_inputs::{
    IntegrationGateInputsEvidence, NativeIntegrationGateInputs, observe_gate_inputs,
};
pub use landed_observation::{
    IntegrationLandedEvidence, NativeIntegrationLanded, observe_landed_owned,
};
mod owner;
mod pending;
mod publication;
mod retained_branch;
pub use retained_branch::{
    RetainedBranchObservation, RetainedBranchOutcome, observe_retained_branch,
};
pub(crate) mod readmission;
pub(crate) use pending::{
    PendingIntegration, retain as retain_conflict_observation, subjects as pending_subjects,
};
pub use publication::{NativePublication, PublicationEvidence, publish_owned};
mod submission;
pub use assembly::{AssemblyEvidence, NativeAssembly, assemble_owned};
pub use owner::IntegrationRecoveryStatus;
pub use owner::gate::{
    CertifiedIntegration, IntegrationCertificationEvidence, IntegrationGateClaim,
};
pub use owner::landing::{IntegrationLandingClaim, IntegrationLandingObservation};
pub(crate) use owner::landing::{
    local_effect_unsettled, validate_intent as validate_landing_intent,
};
pub use owner::publication::{
    AssembledIntegration, PublicationClaim, PublicationEffect, PublishedIntegration,
};
pub(crate) use owner::status_snapshot;
pub use owner::{AssemblyClaim, IntegrationOwner, IntegrationOwnerService, IntegrationPhase};
pub use submission::{
    BoundInspection, BoundIntegrationProposal, CleanIntegrationEvidence, NativeCleanIntegration,
    SubmissionObservation, inspect_submission, observe_clean_submission,
};

/// No single-submission integration is enabled by default or by `[batch]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationPolicy {
    enabled: bool,
    smooth: SmoothPolicy,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    version: u8,
    #[serde(default)]
    enabled: bool,
    publication: Option<String>,
    #[serde(default)]
    smooth: Vec<String>,
}

/// Parse only committed base policy. Enabling requires an explicit distinct-PR
/// publication choice; malformed or unknown settings cannot silently opt in.
pub fn policy_from_pointer(raw: Option<&[u8]>) -> Result<IntegrationPolicy, String> {
    let digest = format!("{:x}", Sha256::digest(raw.unwrap_or_default()));
    let disabled = || IntegrationPolicy {
        enabled: false,
        smooth: SmoothPolicy::default(),
        digest: digest.clone(),
    };
    let Some(raw) = raw else {
        return Ok(disabled());
    };
    let text = std::str::from_utf8(raw)
        .map_err(|e| format!("committed integration pointer is not UTF-8: {e}"))?;
    let pointer: ProjectPointer = toml::from_str(text)
        .map_err(|e| format!("committed integration pointer is invalid: {e}"))?;
    let Some(table) = pointer.integration else {
        return Ok(disabled());
    };
    let table: Table = table
        .try_into()
        .map_err(|e| format!("[integration] policy is invalid: {e}"))?;
    if table.version != 1
        || table
            .publication
            .as_deref()
            .is_some_and(|mode| mode != "managed-pr")
        || (table.enabled && table.publication.as_deref() != Some("managed-pr"))
    {
        return Err("[integration] requires version 1 and explicit publication = \"managed-pr\" when enabled".into());
    }
    let smooth =
        SmoothPolicy::parse(&table.smooth).map_err(|e| e.replace("[batch]", "[integration]"))?;
    if table.enabled && smooth.is_empty() {
        return Err("enabled [integration] requires a nonempty smooth allowlist".into());
    }
    Ok(IntegrationPolicy {
        enabled: table.enabled,
        smooth,
        digest,
    })
}

/// Reviewable pinned plan; serialization is evidence, never effect authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationPlan {
    /// Plan format.
    pub version: u8,
    /// Current pinned integration base and unchanged submitted head.
    pub base: String,
    /// Original submitted PR head, never rewritten by this owner.
    pub head: String,
    /// Git's isolated conflicted merge before supported resolutions.
    pub conflicted_tree: String,
    /// Digest of the base's complete committed policy bytes.
    pub policy: String,
    /// Exact built-in resolution semantics.
    pub strategy: String,
    /// Only these paths may differ from the conflicted merge.
    pub files: Vec<IntegrationFile>,
}

/// A source-preserving pure insertion resolution and its retained blob inputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationFile {
    /// Allowlisted non-code path, additionally checked against the deny floor.
    pub path: String,
    /// Common source blob.
    pub base: String,
    /// Current integration-side blob.
    pub ours: String,
    /// Original submitted blob.
    pub theirs: String,
    /// SHA-256 of deterministic united bytes.
    pub resolved_sha256: String,
}

/// An opaque native proposal. Public JSON cannot become this capability.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::IntegrationProposal;
/// let _: IntegrationProposal = serde_json::from_str("{}").unwrap();
/// ```
pub struct IntegrationProposal {
    plan: IntegrationPlan,
    files: Vec<SmoothedFile>,
    objects: PrivateTrialMerger,
}

impl IntegrationProposal {
    /// Read-only evidence for reserving an integration owner before any effect.
    #[must_use]
    pub fn plan(&self) -> &IntegrationPlan {
        &self.plan
    }

    /// Resolved bytes are inspectable but grant no publication or certification.
    #[must_use]
    pub fn files(&self) -> &[SmoothedFile] {
        &self.files
    }

    /// Explicitly settle private object custody before treating this inspection
    /// as complete. Cleanup failure is an error, never a publishable receipt.
    pub fn settle(self) -> Result<IntegrationPlan, AppError> {
        self.objects.close()?;
        Ok(self.plan)
    }
}

/// Native inspection cannot certify, publish, change a ref, or release a hold.
pub enum Inspection {
    /// Ordinary Git merge needs no smoothing.
    Clean {
        /// Exact ordinary merge tree; still requires central certification.
        tree: String,
    },
    /// Disabled, unsupported, or semantically ambiguous: retain the submission.
    Held {
        /// Concrete policy or semantic constraint.
        reason: String,
    },
    /// Exact deterministic insertion-only proposal in private object custody.
    Proposed(IntegrationProposal),
}

/// Inspect exact parents under one bounded deadline. No author index, worktree,
/// repository object, or ref is changed, including on cancellation/refusal.
pub fn inspect(
    checkout: &Path,
    base: &str,
    head: &str,
    deadline: Instant,
    cancellation: Cancellation,
) -> Result<Inspection, AppError> {
    require_pinned(base, "single integration")?;
    require_pinned(head, "single integration")?;
    let mut objects = PrivateTrialMerger::open_controlled(checkout, deadline, cancellation)?;
    let bytes = objects.file(base, batch_smoothing::POINTER)?;
    let policy = policy_from_pointer(bytes.as_deref()).map_err(AppError::Validation)?;
    if !policy.enabled {
        objects.close()?;
        return Ok(Inspection::Held {
            reason: "single-submission integration recovery is disabled in the pinned base".into(),
        });
    }
    let shape = match objects.merge(base, head)? {
        TrialMerge::Clean { tree } => {
            objects.close()?;
            return Ok(Inspection::Clean { tree });
        }
        TrialMerge::Conflict { shape, .. } => shape,
    };
    let files = match batch_smoothing::classify(&shape, &mut objects) {
        Classification::UnionSmoothable(files)
            if batch_smoothing::admits_all(&policy.smooth, &files) =>
        {
            files
        }
        Classification::UnionSmoothable(_) => {
            objects.close()?;
            return Ok(Inspection::Held {
                reason: "a conflict path is absent from the pinned [integration] smooth allowlist"
                    .into(),
            });
        }
        Classification::AgentCandidate(reason) | Classification::NotSmoothable(reason) => {
            objects.close()?;
            return Ok(Inspection::Held {
                reason: format!("integration needs an explicit semantic decision: {reason}"),
            });
        }
    };
    // A broad operator allowlist must not widen this single-submission
    // capability to source code or unknown executable/configuration formats.
    if let Some(file) = files
        .iter()
        .find(|file| !supported_non_code_path(&file.path))
    {
        let reason = format!(
            "{} is outside single integration's supported non-code paths (.md, .txt, .rst, .gitignore)",
            file.path
        );
        objects.close()?;
        return Ok(Inspection::Held { reason });
    }
    let plan = IntegrationPlan {
        version: 1,
        base: base.into(),
        head: head.into(),
        conflicted_tree: shape.tree,
        policy: policy.digest,
        strategy: STRATEGY.into(),
        files: files
            .iter()
            .map(|file| IntegrationFile {
                path: file.path.clone(),
                base: file.base.clone(),
                ours: file.ours.clone(),
                theirs: file.theirs.clone(),
                resolved_sha256: format!("{:x}", Sha256::digest(file.resolved.as_bytes())),
            })
            .collect(),
    };
    Ok(Inspection::Proposed(IntegrationProposal {
        plan,
        files,
        objects,
    }))
}

#[cfg(test)]
mod tests;

// This closed scope supplements, never replaces, the shared deny floor,
// pinned allowlist, blob checks and insertion-only conflict classification.
fn supported_non_code_path(path: &str) -> bool {
    let path = Path::new(path);
    path.file_name().and_then(|name| name.to_str()) == Some(".gitignore")
        || matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("md" | "txt" | "rst")
        )
}

/// Current new-work authority for the centrally owned native lifecycle. A local
/// policy hint never substitutes for the base-pinned native proposal.
pub(crate) fn candidate_permitted(
    tx: &impl crate::store::ReadOps,
    candidate: &super::VerificationCandidate,
) -> Result<bool, crate::store::StoreError> {
    match owner::check_candidate(tx, candidate) {
        Ok(()) => Ok(true),
        Err(crate::store::StoreError::Validation(_)) => Ok(false),
        Err(error) => Err(error),
    }
}
