//! A clean native merge is an observation, never a gate or hold-release grant.
use super::{SubmissionObservation, read_submission};
use crate::{
    env::Environment,
    error::AppError,
    process::Cancellation,
    service::{
        VerificationCandidate, batch_smoothing,
        integration_recovery::policy_from_pointer,
        trial_merge::{BlobSource, PrivateTrialMerger, TrialMerge, TrialMerger, require_pinned},
    },
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Serializable observations cannot reconstruct native readmission authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanIntegrationEvidence {
    /// Evidence format.
    pub version: u8,
    /// Exact original PR, unchanged submitted head and current integration base.
    pub submission: SubmissionObservation,
    /// Actual ordinary merge tree, still requiring fresh central certification.
    pub tree: String,
    /// SHA-256 of the complete committed pointer bytes at the observed base.
    pub policy: String,
}

/// Minted only after native clean inspection and explicit private cleanup.
/// No cloning, deserialization, certification or hold-release API exists here.
pub struct NativeCleanIntegration {
    evidence: CleanIntegrationEvidence,
    deadline: Instant,
    cancellation: Cancellation,
}

impl NativeCleanIntegration {
    /// The caller must separately prove exact central slot and Store authority.
    pub fn check_live(&self) -> Result<(), AppError> {
        check_live(self.deadline, &self.cancellation)
    }

    /// A live boolean cannot substitute for the exact original central token.
    pub(crate) fn check_owner(&self, cancellation: &Cancellation) -> Result<(), AppError> {
        if !self.cancellation.same_owner(cancellation) {
            return Err(refuse(
                "observation belongs to a different cancellation owner",
            ));
        }
        self.check_live()
    }

    /// Evidence alone grants no future effect, including after this value expires.
    #[must_use]
    pub fn evidence(&self) -> &CleanIntegrationEvidence {
        &self.evidence
    }
}

/// Reobserve an unchanged original submission, prove a clean native merge under
/// its current base's enabled policy, settle private objects, then reobserve
/// the same PR. The original operation deadline and cancellation never renew.
pub fn observe_clean_submission(
    candidate: &VerificationCandidate,
    retained_head: &str,
    environment: &Environment,
    deadline: Instant,
    cancellation: Cancellation,
) -> Result<NativeCleanIntegration, AppError> {
    observe_with(retained_head, deadline, cancellation.clone(), || {
        read_submission(candidate, environment, deadline, &cancellation)
    })
}

fn observe_with(
    retained_head: &str,
    deadline: Instant,
    cancellation: Cancellation,
    mut read: impl FnMut() -> Result<SubmissionObservation, AppError>,
) -> Result<NativeCleanIntegration, AppError> {
    check_live(deadline, &cancellation)?;
    require_pinned(retained_head, "retained clean integration head")?;
    let submission = read()?;
    check_live(deadline, &cancellation)?;
    if submission.head != retained_head {
        return Err(refuse("original submitted head changed"));
    }
    require_pinned(&submission.base, "clean integration base")?;
    let (tree, policy) = inspect_clean(&submission, deadline, &cancellation)?;
    check_live(deadline, &cancellation)?;
    let current = read()?;
    check_live(deadline, &cancellation)?;
    if current != submission {
        return Err(refuse(
            "original PR or current merge inputs changed after inspection",
        ));
    }
    let native = NativeCleanIntegration {
        evidence: CleanIntegrationEvidence {
            version: 1,
            submission,
            tree,
            policy,
        },
        deadline,
        cancellation,
    };
    native.check_live()?;
    Ok(native)
}

fn inspect_clean(
    submission: &SubmissionObservation,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(String, String), AppError> {
    let mut objects =
        PrivateTrialMerger::open_controlled(&submission.checkout, deadline, cancellation.clone())?;
    let result = (|| {
        let bytes = objects.file(&submission.base, batch_smoothing::POINTER)?;
        let policy = policy_from_pointer(bytes.as_deref()).map_err(AppError::Validation)?;
        if !policy.enabled {
            return Err(refuse(
                "integration recovery is disabled in the pinned base",
            ));
        }
        let TrialMerge::Clean { tree } = objects.merge(&submission.base, &submission.head)? else {
            return Err(refuse(
                "original submission still conflicts; smoothing is not a clean proof",
            ));
        };
        require_pinned(&tree, "native clean merge tree")?;
        Ok((tree, policy.digest))
    })();
    // Close explicitly on both success and refusal. Neither a failed merge nor
    // a cleanup failure may reach the second PR read or capability constructor.
    settled(result, objects.close())
}

fn settled<T>(result: Result<T, AppError>, cleanup: Result<(), AppError>) -> Result<T, AppError> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(AppError::Storage(format!(
            "{error}; clean integration private cleanup failed: {cleanup}"
        ))),
    }
}

fn check_live(deadline: Instant, cancellation: &Cancellation) -> Result<(), AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(refuse(
            "original observation expired or its owner cancelled",
        ));
    }
    Ok(())
}

fn refuse(detail: &str) -> AppError {
    AppError::Validation(format!("native clean integration: {detail}"))
}

/// Test siblings may substitute metadata only; native merge and cleanup remain.
#[cfg(test)]
pub(in crate::service::integration_recovery) fn observe_clean_for_fixture(
    retained_head: &str,
    deadline: Instant,
    cancellation: Cancellation,
    read: impl FnMut() -> Result<SubmissionObservation, AppError>,
) -> Result<NativeCleanIntegration, AppError> {
    observe_with(retained_head, deadline, cancellation, read)
}

#[cfg(test)]
mod tests;
