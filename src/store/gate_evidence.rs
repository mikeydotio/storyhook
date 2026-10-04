//! Durable, non-authoritative evidence for admitted verifier attempts.

use super::{GlobalSeq, ProjectId, StoreError};
use crate::service::gate_cost::Elapsed;
use serde::{Deserialize, Serialize};

/// Identity retained across retries of the same submission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateSubmission {
    /// Store project identity.
    pub project: ProjectId,
    /// Display story identity.
    pub story_id: String,
    /// Verifying transition; unavailable for legacy candidates.
    pub generation: Option<GlobalSeq>,
    /// Original queue-entry time, never replaced by retry admission.
    pub submitted_at: Option<String>,
}

/// A measured producer interval; a missing end is not zero work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInterval {
    /// Producer-scoped stable identity, unique within an attempt.
    pub id: String,
    /// Checklist leg or lifecycle path.
    pub path: String,
    /// Work classification (workspace, resource-wait, discovery, compile-link, execution, cleanup, verdict).
    pub phase: String,
    /// UTC start when reported.
    pub started_at: Option<String>,
    /// UTC end when reported.
    pub ended_at: Option<String>,
    /// Measured wall duration, in milliseconds.
    pub milliseconds: Option<u64>,
    /// Whether a duration was reconstructed from UTC boundaries.
    pub estimated: bool,
    /// Producer monotonic start sample, used only with its matching end.
    pub started_monotonic_ns: Option<u64>,
}

/// A named failing case observed by a runner, not a causal attribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateFailedCase {
    /// Suite or leg path.
    pub path: String,
    /// Exact case identity; missing in legacy producer output.
    pub name: Option<String>,
    /// Test executable, file or browser project, when known.
    pub target: Option<String>,
    /// Runner-provided unique identifier, where the runner has one.
    pub identity: Option<String>,
    /// Original title hierarchy, avoiding display-delimiter ambiguity.
    pub title_path: Option<Vec<String>>,
}

/// One leg's execution and cache evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateLeg {
    /// Checklist path.
    pub path: String,
    /// Executed result, reused, skipped, pending or running.
    pub status: String,
    /// Observed leg duration, never invented for reuse.
    pub milliseconds: Option<u64>,
    /// Receipt fingerprint used for this leg, if supplied.
    pub receipt: Option<String>,
}

/// Immutable inputs established by the gate, with explicit unavailable values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInputs {
    /// Submitted head commit.
    pub head: Option<String>,
    /// Pinned base commit.
    pub base: Option<String>,
    /// Proposed merge tree.
    pub tree: Option<String>,
    /// Gate contract identity and command.
    pub contract: Option<serde_json::Value>,
    /// Platform and toolchain evidence actually collected.
    pub toolchain: Option<serde_json::Value>,
    /// Scheduling/admission policy and observed resource limits.
    pub resources: Option<serde_json::Value>,
    /// Build artifact and validation cache observations.
    pub cache: Option<serde_json::Value>,
}

impl GateInputs {
    /// Known immutable inputs may be completed, but never replaced or erased.
    pub(crate) fn preserved_by(&self, next: &Self) -> bool {
        fn retained<T: PartialEq>(old: &Option<T>, next: &Option<T>) -> bool {
            old.is_none() || old == next
        }
        retained(&self.head, &next.head)
            && retained(&self.base, &next.base)
            && retained(&self.tree, &next.tree)
            && retained(&self.contract, &next.contract)
            && retained(&self.toolchain, &next.toolchain)
            && retained(&self.resources, &next.resources)
            && retained(&self.cache, &next.cache)
    }
}

/// One physical gate within an admission, with its own immutable tree identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateExecution {
    /// Unique physical execution token; never reused by a bisection probe.
    pub id: String,
    /// UTC time before calling the actuator.
    pub started_at: String,
    /// UTC return after supervision and cleanup, absent if interrupted.
    pub finished_at: Option<String>,
    /// Full physical execution wall duration, not the sum of child spans.
    pub milliseconds: Option<u64>,
    /// Whether elapsed duration was reconstructed after restart.
    pub estimated: bool,
    /// Independently observed result, separate from the admission budget.
    pub verdict: Option<String>,
    /// Input identity and environment as they become known.
    pub inputs: GateInputs,
    /// Ordered host admission observations, separate from immutable launch inputs.
    #[serde(default)]
    pub resource_events: Vec<serde_json::Value>,
    /// Producer intervals, which may overlap and must not be added as wall time.
    pub intervals: Vec<GateInterval>,
    /// Latest observed state of each gate leg.
    pub legs: Vec<GateLeg>,
    /// Exact failed cases; absent names remain unknown.
    pub failed_cases: Vec<GateFailedCase>,
    /// Raw output and archived journal references.
    pub logs: Vec<String>,
    /// Evidence errors with their original context.
    pub diagnostics: Vec<String>,
    /// Live or archived journal file, bound by its run marker.
    pub journal_path: String,
    /// Number of complete bytes imported from this journal.
    pub journal_offset: u64,
    /// Whether the journal run marker matched this admission and execution.
    pub journal_bound: bool,
    /// Every submission using this physical execution, without cost division.
    pub submissions: Vec<GateSubmission>,
}

impl GateExecution {
    /// Starts a physical observation without asserting a result or unknown inputs.
    pub fn new(id: String, at: &str, journal_path: String) -> Self {
        Self {
            id,
            started_at: at.into(),
            finished_at: None,
            milliseconds: None,
            estimated: false,
            verdict: None,
            inputs: GateInputs::default(),
            resource_events: vec![],
            intervals: vec![],
            legs: vec![],
            failed_cases: vec![],
            logs: vec![],
            diagnostics: vec![],
            journal_path,
            journal_offset: 0,
            journal_bound: false,
            submissions: vec![],
        }
    }

    /// Completed executions are immutable; live ones may only add evidence.
    pub(crate) fn preserved_by(&self, next: &Self) -> bool {
        if self.finished_at.is_some() {
            return self == next;
        }
        self.id == next.id
            && self.started_at == next.started_at
            && self.submissions == next.submissions
            && self.inputs.preserved_by(&next.inputs)
            && next.resource_events.starts_with(&self.resource_events)
            && next.journal_offset >= self.journal_offset
            && next.failed_cases.starts_with(&self.failed_cases)
            && self.logs.iter().all(|log| next.logs.contains(log))
            && self
                .diagnostics
                .iter()
                .all(|d| next.diagnostics.contains(d))
            && (!self.estimated || next.estimated)
            && self
                .milliseconds
                .is_none_or(|ms| next.milliseconds.is_some_and(|n| n >= ms))
            && intervals_preserved(&self.intervals, &next.intervals)
    }
}

pub(crate) fn intervals_preserved(old: &[GateInterval], next: &[GateInterval]) -> bool {
    old.iter().all(|span| {
        next.iter().any(|new| {
            new.id == span.id
                && new.path == span.path
                && new.phase == span.phase
                && new.started_at == span.started_at
                && new.started_monotonic_ns == span.started_monotonic_ns
                && (span.ended_at.is_none() || new == span)
        })
    })
}

/// One persisted admission. It grants neither execution ownership nor certification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateAttempt {
    /// Evidence protocol version.
    pub version: u32,
    /// Unique verifier attempt token.
    pub id: String,
    /// Submission identity shared by retries.
    pub submission: GateSubmission,
    /// Previous attempt of this story, including superseded submissions.
    pub previous_attempt: Option<String>,
    /// Compare-and-swap revision.
    pub revision: i64,
    /// UTC admission, before preparation or resource waits.
    pub admitted_at: String,
    /// UTC completion; absent while live or unresolved after interruption.
    pub finished_at: Option<String>,
    /// Total admission-to-verdict elapsed observation.
    pub elapsed: Elapsed,
    /// Final independently observed gate result; never inferred from elapsed time.
    pub verdict: Option<String>,
    /// Admission lifecycle intervals, independent of nested physical executions.
    pub intervals: Vec<GateInterval>,
    /// Physical gates, in execution order. A batch probe never resets the budget.
    pub executions: Vec<GateExecution>,
    /// Current preparation journal, replaced by a synced archive at finalization.
    #[serde(default)]
    pub journal_path: Option<String>,
    /// Data loss, clock anomalies or evidence diagnostics outside one execution.
    pub diagnostics: Vec<String>,
}

impl GateAttempt {
    /// Starts a new observation without asserting unknown gate inputs.
    pub fn new(id: String, submission: GateSubmission, at: &str) -> Self {
        Self {
            version: 1,
            id,
            submission,
            previous_attempt: None,
            revision: 0,
            admitted_at: at.into(),
            finished_at: None,
            elapsed: Elapsed::new(at),
            verdict: None,
            intervals: vec![],
            executions: vec![],
            journal_path: None,
            diagnostics: vec![],
        }
    }

    /// Validates the stored identity before writing or trusting decoded data.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1
            || self.id.is_empty()
            || self.submission.story_id.is_empty()
            || self.revision < 0
            || chrono::DateTime::parse_from_rfc3339(&self.admitted_at).is_err()
            || chrono::DateTime::parse_from_rfc3339(&self.elapsed.checkpoint_at).is_err()
            || self
                .finished_at
                .as_ref()
                .is_some_and(|at| chrono::DateTime::parse_from_rfc3339(at).is_err())
            || self
                .submission
                .submitted_at
                .as_ref()
                .is_some_and(|at| chrono::DateTime::parse_from_rfc3339(at).is_err())
            || (self.elapsed.milliseconds >= crate::service::gate_cost::BUDGET_MS)
                != self.elapsed.breached_at.is_some()
        {
            return Err(StoreError::Validation(format!(
                "invalid gate evidence {}",
                self.id
            )));
        }
        let mut ids = std::collections::BTreeSet::new();
        for execution in &self.executions {
            for event in &execution.resource_events {
                super::gate_resources::validate(
                    event,
                    &self.id,
                    &execution.id,
                    self.submission.generation.map(|g| g.get()),
                )
                .map_err(StoreError::Validation)?;
            }
            super::gate_resources::ordered(&execution.resource_events)
                .map_err(StoreError::Validation)?;
            if execution.id.is_empty()
                || !ids.insert(&execution.id)
                || chrono::DateTime::parse_from_rfc3339(&execution.started_at).is_err()
                || execution
                    .finished_at
                    .as_ref()
                    .is_some_and(|at| chrono::DateTime::parse_from_rfc3339(at).is_err())
                || execution
                    .submissions
                    .iter()
                    .any(|s| s.project != self.submission.project || s.story_id.is_empty())
            {
                return Err(StoreError::Validation(format!(
                    "invalid gate execution {} in {}",
                    execution.id, self.id
                )));
            }
        }
        Ok(())
    }

    /// Process result, separate from whether completed tests certified a tree.
    pub fn budget_status(&self) -> &'static str {
        if self.elapsed.breached_at.is_some() {
            "process-budget-breach"
        } else if self.elapsed.diagnostic.is_some() {
            "unknown"
        } else if self.finished_at.is_some() {
            "within-budget"
        } else {
            "observing"
        }
    }
}
