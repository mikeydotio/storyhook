//! Durable recovery receipts, separate from incidents and process ownership.

use serde::{Deserialize, Serialize};

use super::{GlobalSeq, VerificationIncident};

/// Latest operator acknowledgement and recovery request for one project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRecovery {
    /// Preserved even when a later start replaces the recovery request.
    pub acknowledgement: Option<VerificationAcknowledgementRecord>,
    /// Latest request; pending requests survive daemon restart.
    pub request: Option<VerificationRecoveryRequest>,
}

/// The incident evidence and actual permission chosen by an acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationAcknowledgementRecord {
    /// Exact incident that was cleared.
    pub incident: VerificationIncident,
    /// RFC3339 acknowledgement time.
    pub at: String,
    /// Explicit action, distinct from the resulting permission.
    pub action: VerificationAcknowledgementIntent,
    /// Admission permission committed with the acknowledgement.
    pub enabled: bool,
}

/// Whether acknowledgement changes admission or preserves legacy permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationAcknowledgementIntent {
    /// Explicitly enable admission.
    Retry,
    /// Explicitly disable admission.
    LeaveStopped,
    /// The REST request omitted an action.
    PreserveAdmission,
}

/// A correlated request to the existing per-project verifier worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRecoveryRequest {
    /// Unique identity, independent of story generation and timestamp precision.
    pub id: String,
    /// RFC3339 request time.
    pub requested_at: String,
    /// Admission evidence or a concrete explanation for no admission.
    pub outcome: VerificationRecoveryOutcome,
    /// Actual admission retained after completion or interruption.
    pub admission: Option<VerificationAdmission>,
}

/// Recovery admission evidence; an admitted attempt is never a gate verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum VerificationRecoveryOutcome {
    /// Committed before the worker is woken.
    Scheduled,
    /// Live ownership was acquired by the worker.
    Admitted,
    /// The tick settled, with or without an admitted attempt.
    Settled {
        /// Stable reason: empty-queue, stopped, halted, interrupted, or failure.
        reason: String,
        /// Context needed to diagnose the outcome.
        detail: String,
    },
}

impl VerificationRecovery {
    /// Registers fresh intent without deleting the latest acknowledgement.
    pub fn schedule(&mut self, now: &str) {
        self.request = Some(VerificationRecoveryRequest {
            id: uuid::Uuid::new_v4().to_string(),
            requested_at: now.into(),
            outcome: VerificationRecoveryOutcome::Scheduled,
            admission: None,
        });
    }

    /// Resolves only pending intent; never rewrites evidence of admission.
    pub fn settle_pending(&mut self, reason: &str, detail: &str) {
        if let Some(request) = &mut self.request
            && request.outcome == VerificationRecoveryOutcome::Scheduled
        {
            request.outcome = VerificationRecoveryOutcome::Settled {
                reason: reason.into(),
                detail: detail.into(),
            };
        }
    }
}

/// Identity of the attempt actually caused by a recovery request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationAdmission {
    /// Unique attempt identifier.
    pub attempt_id: String,
    /// Actual queue candidate admitted.
    pub story_id: String,
    /// Its verification generation.
    pub generation: Option<GlobalSeq>,
    /// RFC3339 acquisition time.
    pub started_at: String,
}
