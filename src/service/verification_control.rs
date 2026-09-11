//! Manual verifier intent, independent of infrastructure incidents.

use crate::error::AppError;
use crate::store::{ProjectId, StoreError, VerificationIncident, WriteOps};

/// An operator command for one project's verifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationAction {
    /// Permit admission after the previous owned attempt exits.
    Start,
    /// Prevent admission and finish the owned attempt.
    Drain,
    /// Prevent admission and cancel the owned attempt.
    Stop,
}

/// Explicit acknowledgement intent; omission retains legacy permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationAcknowledgement {
    /// Acknowledge and permit a new attempt.
    Retry,
    /// Acknowledge and keep admissions disabled.
    LeaveStopped,
}

/// Validates and clears the exact current incident under the caller's write
/// lock, so stale requests cannot acknowledge or alter a newer failure.
pub(crate) fn acknowledge_in_transaction(
    tx: &mut impl WriteOps,
    project: ProjectId,
    incident_id: &str,
) -> Result<VerificationIncident, StoreError> {
    let current = tx.verification_incident(project)?.ok_or_else(|| {
        AppError::Validation("no verification incident is active for this project".into())
    })?;
    if !current.halted {
        return Err(
            AppError::Validation("the verification incident is still retrying".into()).into(),
        );
    }
    if current.incident_id != incident_id {
        return Err(AppError::Validation(format!(
            "verification incident `{incident_id}` is stale; current incident is `{}`",
            current.incident_id
        ))
        .into());
    }
    if !tx.clear_verification_incident(incident_id)? {
        return Err(AppError::Storage(format!("current verification incident `{incident_id}` disappeared inside its acknowledgement transaction")).into());
    }
    Ok(current)
}
