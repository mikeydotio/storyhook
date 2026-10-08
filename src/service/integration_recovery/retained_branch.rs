//! Read-only managed branch retention facts. No deletion, merge or cleanup authority.
//!
//! This intentionally accepts historical assembly *scalar identities* so an
//! uncertain publication or completed landing can still be observed. It never
//! opens the historical private workspace or reconstructs its native custody.
use super::{AssemblyEvidence, publication};
use crate::{
    env::Environment,
    error::AppError,
    github_access::{Repository, private_fetch::full_oid},
    process::Cancellation,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// A point-in-time diagnostic only. Deserialization cannot authorize any effect.
/// Main owns exact durable owner/submission binding before and after observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedBranchObservation {
    pub version: u8,
    pub owner: String,
    pub assembly_epoch: u32,
    pub repository: String,
    pub reference: String,
    pub expected_head: String,
    pub observed_at: String,
    pub outcome: RetainedBranchOutcome,
}

/// Every outcome is read-only: absence is not proof of merging, and moved refs
/// are preserved just as exact refs are. Unknown never implies remote absence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetainedBranchOutcome {
    Absent,
    RetainedExact,
    MovedPreserved { observed_head: String },
    Unknown { detail: String },
}

/// Observe one exact managed ref under a fresh bounded lifetime. No retries,
/// writes, branch deletions, original-PR state inference or child-lifetime renewal.
/// Failures, ambiguous output and expiry/cancellation are diagnostic Unknown.
pub fn observe_retained_branch(
    env: &Environment,
    assembly: &AssemblyEvidence,
    deadline: Instant,
    cancellation: &Cancellation,
) -> RetainedBranchObservation {
    let mut observation = observe_with(assembly, deadline, cancellation, |reference| {
        let cancelled = || cancellation.is_cancelled();
        let repository = Repository::resolve_publication(
            &assembly.submission.checkout,
            env,
            &assembly.submission.repository,
            deadline,
            &cancelled,
        )?;
        // Existing protected transport rechecks the resolved origin, URL
        // rewrites and credentials under the same quiescent absolute lifetime.
        repository.git_publication(
            &[
                "ls-remote".into(),
                "--heads".into(),
                "origin".into(),
                reference.into(),
            ],
            None,
            deadline,
            &cancelled,
        )
    });
    observation.observed_at = env.now();
    // Timestamping is not a way to rescue an observation whose original
    // cancellation/deadline ended after capture and strict parsing.
    if let Err(error) = live(deadline, cancellation) {
        observation.outcome = unknown(error);
    }
    observation
}

fn observe_with(
    assembly: &AssemblyEvidence,
    deadline: Instant,
    cancellation: &Cancellation,
    read: impl FnOnce(&str) -> Result<Vec<u8>, AppError>,
) -> RetainedBranchObservation {
    let reference = format!("refs/heads/{}", assembly.branch);
    let result = (|| {
        live(deadline, cancellation)?;
        if uuid::Uuid::parse_str(&assembly.owner).is_err()
            || !assembly
                .owner
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
            || assembly.branch != format!("storyhook/integration/{}", assembly.owner)
            || !full_oid(&assembly.commit)
            || !assembly.submission.checkout.is_absolute()
        {
            return Err(refuse("invalid immutable managed branch binding"));
        }
        let answer = read(&reference)?;
        live(deadline, cancellation)?;
        let head = publication::parse_remote_head(&answer, &reference)?;
        let outcome = match head {
            None => RetainedBranchOutcome::Absent,
            Some(head) if head == assembly.commit => RetainedBranchOutcome::RetainedExact,
            Some(observed_head) => RetainedBranchOutcome::MovedPreserved { observed_head },
        };
        live(deadline, cancellation)?;
        Ok(outcome)
    })();
    RetainedBranchObservation {
        version: 1,
        owner: assembly.owner.clone(),
        assembly_epoch: assembly.epoch,
        repository: assembly.submission.repository.clone(),
        reference,
        expected_head: assembly.commit.clone(),
        observed_at: String::new(),
        outcome: result.unwrap_or_else(unknown),
    }
}
fn live(deadline: Instant, cancellation: &Cancellation) -> Result<(), AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        Err(refuse("branch observation expired or was cancelled"))
    } else {
        Ok(())
    }
}
fn unknown(error: AppError) -> RetainedBranchOutcome {
    RetainedBranchOutcome::Unknown {
        detail: crate::daemon::crash::redact(&error.to_string())
            .chars()
            .take(2048)
            .collect(),
    }
}
fn refuse(detail: &str) -> AppError {
    AppError::Validation(format!("managed branch observation unknown: {detail}"))
}

#[cfg(test)]
mod tests;
