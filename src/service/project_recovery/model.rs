//! Versioned coordination state; immutable observations live in their own rows.

use super::super::{project_fault::ProjectFault, verification::VerificationCandidate};
use crate::store::{GlobalSeq, ProjectRecovery, ProjectRecoveryObservation, StoryNo};
use serde::{Deserialize, Serialize};

/// Assessment transport state, separate from both story and verifier ownership.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssessmentStatus {
    /// The coordinator may claim a delivery attempt after checking authority.
    Pending,
    /// A claimed external operation needs completion or restart reconciliation.
    InFlight,
    /// A managed agent received the charter and must provide its decision.
    Delivered,
    /// A valid decision proves receipt and ends the assessment deadline.
    Decided,
    /// Delivery, policy, or response timeout requires explicit resolution.
    Held,
}

/// Structured reason automatic assessment cannot proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssessmentHold {
    /// The operator disabled verifier admission.
    OperatorStop,
    /// A human-only or no-auto label reserves the story.
    ReservedLabel,
    /// A later state, generation, dependency episode, or label reservation revoked authority.
    AuthorityChanged,
    /// The originating story was removed.
    SubjectMissing,
    /// A dependency, reset, or unresolved landing prevents effects.
    ResourceOrDependency,
    /// Three delivery attempts have been proven unsuccessful.
    DeliveryExhausted,
    /// Three changed committed repair inputs completed without resolving the fault.
    RepairExhausted,
    /// The managed runtime cannot prove that replacement is safe.
    OwnershipUncertain,
    /// A delivered assessor did not decide within thirty minutes.
    ResponseExpired,
}

impl AssessmentHold {
    /// Human diagnosis; callers classify by the enum rather than this text.
    pub fn detail(self) -> &'static str {
        match self {
            Self::OperatorStop => "manual verifier stop prevents automatic recovery",
            Self::ReservedLabel => "human-only or no-auto reservation prevents automatic recovery",
            Self::AuthorityChanged => {
                "assessment story state, generation, or reservation authority changed"
            }
            Self::SubjectMissing => "assessment story no longer exists",
            Self::ResourceOrDependency => {
                "assessment story has a dependency, reset, or unresolved landing hold"
            }
            Self::DeliveryExhausted => "three proven assessment deliveries failed",
            Self::RepairExhausted => {
                "three changed repair submissions completed without resolving this recovery"
            }
            Self::OwnershipUncertain => "assessment ownership remains uncertain",
            Self::ResponseExpired => {
                "scope assessment exceeded its 30-minute response deadline; do not launch a competing agent"
            }
        }
    }
}

/// Correlation and bounded delivery evidence for the scope assessor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    /// Stable token included in the charter and required by the decision input.
    pub dispatch_identity: String,
    /// Origin story selected to assess scope.
    pub story: StoryNo,
    /// Exact origin generation that found the fault.
    pub generation: GlobalSeq,
    /// Current transport state.
    pub status: AssessmentStatus,
    /// Machine-readable hold, present exactly when status is held.
    pub hold: Option<AssessmentHold>,
    /// Delivery attempt ordinal; stale completions cannot settle a later attempt.
    pub epoch: u32,
    /// Proven failed deliveries, excluding ambiguous or merely claimed effects.
    pub failures: u8,
    /// RFC3339 time at which the current delivery was claimed.
    pub started_at: Option<String>,
    /// RFC3339 time at which delivery was confirmed.
    pub delivered_at: Option<String>,
    /// Retained delivery or policy diagnosis.
    pub detail: String,
    /// Exact most recent completion, so identical transport replay is idempotent.
    pub last_result: Option<AssessmentDelivery>,
}

/// Current authority retained for one affected submission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedSubmission {
    /// Original queue evidence, including source and cleanup lease.
    pub candidate: VerificationCandidate,
    /// Project-local numeric story identity.
    pub story: StoryNo,
    /// Latest state-change event recorded by enrollment.
    pub state_revision: GlobalSeq,
    /// Latest event reserving either human-only or no-auto, including removed labels.
    pub label_revision: Option<GlobalSeq>,
    /// Whether enrollment returned this story from verification to its agent.
    pub returned: bool,
}

/// Exact awaiting event created by a terminal assessment, never a blanket unblock grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedAssessmentHold {
    /// Subject whose original submission remains unjudged.
    pub story: StoryNo,
    /// Original verification generation.
    pub generation: GlobalSeq,
    /// Machine-readable terminal assessment cause.
    pub cause: AssessmentHold,
    /// Exact reason committed to the story.
    pub awaiting: String,
    /// Global sequence of this recovery's awaiting write.
    pub event: GlobalSeq,
}

/// Versioned state revised independently of immutable gate observations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryState {
    /// State schema version; unsupported versions fail closed.
    pub version: u8,
    /// RFC3339 creation time.
    pub created_at: String,
    /// RFC3339 most recent coordination change.
    pub updated_at: String,
    /// Affected submissions; new observations do not create competing assessors.
    pub subjects: Vec<AffectedSubmission>,
    /// Scope assessment owner and durable delivery intent.
    pub assessment: Assessment,
    /// Exact accepted request and resulting work; absent until scope is decided.
    #[serde(default)]
    pub decision: Option<super::DecisionReceipt>,
    /// Terminal assessment holds owned by exact event identity.
    #[serde(default)]
    pub holds: Vec<OwnedAssessmentHold>,
    /// Durable managed repair and affected-agent resume effects.
    #[serde(default)]
    pub work: Vec<super::WorkDelivery>,
    /// Pinned repair attempts; interruption alone consumes no completion budget.
    #[serde(default)]
    pub attempts: Vec<super::RepairAttempt>,
    /// Exact refused admissions, retained without executing another gate.
    #[serde(default)]
    pub refusals: Vec<super::RepairRefusalRecord>,
    /// Confirmed landing authority; story closure alone cannot release recovery.
    #[serde(default)]
    pub landing: Option<super::RepairLanding>,
    /// Original incidents converted only with matching typed fault observations.
    #[serde(default)]
    pub legacy_incidents: Vec<crate::store::VerificationIncident>,
}

/// Exact structured evidence originally produced by the verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultObservation {
    /// Evidence schema version.
    pub version: u8,
    /// Source, generation, and resource authority at the time of verification.
    pub candidate: VerificationCandidate,
    /// Proven project fault; the tree remains unjudged.
    pub fault: ProjectFault,
}

/// Read-only view returned to recovery orchestration and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryView {
    /// Stable storage identity and revision.
    pub record: ProjectRecovery,
    /// Validated, versioned coordination state.
    pub state: RecoveryState,
    /// Original observations, never overwritten by a newer submission.
    pub observations: Vec<ProjectRecoveryObservation>,
}

/// A transport completion whose classification comes from the managed runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "detail", rename_all = "kebab-case")]
pub enum AssessmentDelivery {
    /// The assessor received its charter.
    Delivered,
    /// Delivery is proven absent or failed; bounded retry cannot duplicate a live agent.
    ProvenFailure(String),
    /// A live or ambiguous owner must not be replaced automatically.
    Uncertain(String),
}

/// A dependency hold belongs to one returned submission and one exact awaiting event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedDependencyHold {
    /// Affected story, never the repair itself.
    pub story: StoryNo,
    /// Original unjudged verification generation.
    pub generation: GlobalSeq,
    /// Exact text committed to the ordinary story hold.
    pub awaiting: String,
    /// Exact awaiting write; a replacement with identical text is independent.
    pub event: GlobalSeq,
}
