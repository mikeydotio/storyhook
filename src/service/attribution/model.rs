//! Versioned observation data. Deserialization does not grant return authority.

use crate::store::{GateInputs, GateSubmission};
use serde::{Deserialize, Serialize};

/// Maximum physical diagnostic starts for one submission, including interrupted starts.
pub const MAX_PROBES: usize = 8;
/// Maximum active diagnosis time for one submission, separate from gate certification.
pub const MAX_DIAGNOSIS_MS: u64 = 300_000;

/// Proven responsibility, distinct from the observed kind of failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureCause {
    /// A controlled contrast identifies a defect in the submitted change.
    CandidateCaused,
    /// The same defect reproduces on the pinned base.
    SharedProject,
    /// Observed tooling, resource or external prerequisite failure.
    HostExternal,
    /// Base movement or merge resolution needs integration work.
    Integration,
    /// Evidence cannot establish responsibility.
    Unknown,
}

/// A separate failing check, including its original evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureComponent {
    /// Unique identifier within the diagnosis.
    pub id: String,
    /// Exact runner case or structural check identity.
    pub check: String,
    /// Stable assertion or diagnostic identity, not a filename heuristic.
    pub signature: String,
    /// Requirement violated by the observation.
    pub requirement: String,
    /// Retained original output, independent of diagnostic output.
    pub log: String,
    /// Direct non-author observations; candidate cause cannot be asserted here.
    pub observed_cause: FailureCause,
}

/// Which controlled input this physical probe executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeSide {
    /// Exact failed candidate merge tree.
    Candidate,
    /// Pinned base or a validated detector-preserving control tree.
    Control,
}

/// Detector relationship established by the diagnostic preparation adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DetectorRelation {
    /// Identical detector and its dependency closure on both trees.
    Unchanged,
    /// Exact retained patch transplants the detector without its production change.
    Transplant {
        /// Content digest of the retained patch.
        patch: String,
    },
    /// Exact retained patch removes the implicated change, preserving the detector.
    Ablation {
        /// Content digest of the retained patch.
        patch: String,
    },
}

/// Immutable plan, validated by a supported adapter before probe reservation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContrastPlan {
    /// Failure component selected for this comparison.
    pub component: String,
    /// Exact candidate merge tree.
    pub candidate_tree: String,
    /// Exact pinned base commit.
    pub base: String,
    /// Exact prepared control tree.
    pub control_tree: String,
    /// Content identity of the assertion and its relevant dependencies.
    pub detector: String,
    /// How the control preserves the detector's meaning.
    pub relation: DetectorRelation,
    /// Exact command and arguments, executed without shell interpolation.
    pub argv: Vec<String>,
}

/// Conditions actually observed for a diagnostic execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeEnvironment {
    /// Toolchain identity, including the executing runtime.
    pub toolchain: String,
    /// Fixture and external input identity.
    pub fixtures: String,
    /// Supported resource policy revision, not a worker count.
    pub resource_policy: String,
    /// Evidence reference for the individual resource grant.
    pub grant: String,
    /// Whether measured conditions stayed within that policy throughout execution.
    pub supported: bool,
}

/// A completed probe; failures to execute are neither passes nor matching failures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ProbeOutcome {
    /// The selected detector executed and passed.
    Passed,
    /// The selected detector executed and failed with this exact signature.
    Failed {
        /// Stable diagnostic identity established by the runner adapter.
        signature: String,
    },
    /// Execution or interpretation failed; responsibility remains uncertain.
    Unavailable {
        /// Originating error with context.
        detail: String,
    },
}

/// Completed evidence for one physical probe, immutable once retained.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeResult {
    /// Actual executed tree, independently checked after preparation.
    pub tree: String,
    /// Actual detector content identity.
    pub detector: String,
    /// Number of executions of the exact selected check; zero is not a pass.
    pub executions: u32,
    /// Interpreted outcome from retained runner output.
    pub outcome: ProbeOutcome,
    /// Observed execution environment; missing means unknown.
    pub environment: Option<ProbeEnvironment>,
    /// Complete retained output path.
    pub log: String,
    /// Retained SH-867 physical execution identifier.
    pub execution_id: String,
    /// Owned descendants and workspace restoration were proved settled.
    pub cleanup_complete: bool,
    /// Active monotonic duration, including preparation and cleanup.
    pub milliseconds: u64,
}

/// A reservation is persisted before launch, so restart cannot refund its allowance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticProbe {
    /// Unique physical operation identifier.
    pub id: String,
    /// Index of the immutable contrast plan.
    pub plan: usize,
    /// Input selected for this execution.
    pub side: ProbeSide,
    /// UTC start for restart diagnostics, never a monotonic clock substitute.
    pub started_at: String,
    /// Absent until execution and cleanup have settled.
    pub completed: Option<ProbeResult>,
}

/// Immutable assessment history; the service derives cause from referenced evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributionAssessment {
    /// Failure being assessed.
    pub component: String,
    /// Revision whose completed evidence was inspected.
    pub evidence_revision: i64,
    /// Derived cause, not an assessor's assertion.
    pub cause: FailureCause,
    /// Exact physical probes supporting the assessment.
    pub probes: Vec<String>,
    /// Explanation of what is proved or still missing.
    pub detail: String,
}

/// Durable diagnosis for a gate outcome, independent of immutable cost records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributionRecord {
    /// Wire schema version; currently one.
    pub version: u8,
    /// Stable diagnosis identity.
    pub id: String,
    /// Compare-and-swap revision.
    pub revision: i64,
    /// Immutable submitted story and generation.
    pub submission: GateSubmission,
    /// Owning verifier admission.
    pub attempt: String,
    /// Exact established gate inputs; unavailable fields cannot prove a return.
    pub inputs: GateInputs,
    /// UTC creation time.
    pub created_at: String,
    /// All independently observed failure components.
    pub components: Vec<FailureComponent>,
    /// Reservation retained before control preparation; absent in legacy observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<super::DiagnosticPreparation>,
    /// Whole-comparison cleanup, absent until the native owner explicitly settles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<super::DiagnosticSettlement>,
    /// Validated, immutable plans, retained in preparation order.
    pub plans: Vec<ContrastPlan>,
    /// Durable physical reservations, retained in execution order.
    pub probes: Vec<DiagnosticProbe>,
    /// Append-only decision history.
    pub assessments: Vec<AttributionAssessment>,
    /// Consumed active diagnosis allowance, including interrupted executions.
    pub diagnosis_ms: u64,
    /// Whether this diagnosis still holds the submitted generation.
    pub held: bool,
    /// Terminal release or supersession explanation, absent while held.
    pub retired: Option<String>,
}

impl AttributionRecord {
    /// Unfinished execution or unproved cleanup prevents any further diagnosis launch.
    pub(crate) fn has_unsettled_diagnosis(&self) -> bool {
        self.has_unsettled_execution()
            || (self.preparation.is_some()
                && self.settlement.as_ref().is_none_or(|s| !s.cleanup_complete))
    }

    /// Per-operation completion permits the live owner to reserve its next probe.
    pub(crate) fn has_unsettled_execution(&self) -> bool {
        self.preparation.as_ref().is_some_and(|p| p.unsettled())
            || self
                .probes
                .iter()
                .any(|p| p.completed.as_ref().is_none_or(|r| !r.cleanup_complete))
    }
}
