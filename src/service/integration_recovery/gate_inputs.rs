//! Fresh native gate inputs, never gate success or landing authority.
//!
//! This operation has a bounded control lifetime. Consuming it does not bound
//! a progressing central gate's runtime or replace its guard/cancellation.
use super::{
    AssemblyEvidence, IntegrationGateClaim, IntegrationOwnerService, PublicationEvidence,
    publication,
};
use crate::{error::AppError, github_access::Repository, process::Cancellation, store::Store};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Observed identities only; deserialization cannot admit or certify a gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationGateInputsEvidence {
    /// Native observation envelope version.
    pub version: u8,
    /// Exact original managed integration owner.
    pub owner: String,
    /// Live original central gate admission.
    pub attempt: String,
    /// Exact native managed and original PR identities.
    pub publication: PublicationEvidence,
    /// Fresh remote base, initially required to equal the pinned base.
    pub current_base: String,
    /// Original intended target branch.
    pub base_branch: String,
    /// Exact native assembled resolution tree.
    pub tree: String,
    /// Pinned committed single-integration policy digest.
    pub policy: String,
    /// Ordered immutable original base and author head.
    pub parents: [String; 2],
}

/// Constructed only by the successful native observation below. Not Clone or
/// Deserialize: saved JSON cannot recreate a fresh input-observation capability.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::NativeIntegrationGateInputs;
/// let _: NativeIntegrationGateInputs = serde_json::from_str("{}").unwrap();
/// ```
pub struct NativeIntegrationGateInputs {
    evidence: IntegrationGateInputsEvidence,
    deadline: Instant,
    cancellation: Cancellation,
}
impl NativeIntegrationGateInputs {
    /// Reviewable inputs, not certification or authority to merge.
    pub fn evidence(&self) -> &IntegrationGateInputsEvidence {
        &self.evidence
    }

    /// Check the original observation lifetime and exact live native claimant.
    /// The controller must additionally CAS its retained authority/live admission
    /// in the same transaction which accepts these inputs into a running gate.
    pub fn validate_for(&self, claim: &IntegrationGateClaim) -> Result<(), AppError> {
        self.validate_observation(
            claim.id(),
            claim.attempt(),
            claim.publication(),
            claim.assembly(),
        )?;
        self.finish_custody(|| claim.validate_custody())
    }

    fn finish_custody(
        &self,
        custody: impl FnOnce() -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        custody()?;
        // Native filesystem validation can outlast or observe cancellation of
        // this independent observation even while the claim remains live.
        live(self.deadline, &self.cancellation)
    }

    fn validate_observation(
        &self,
        owner: &str,
        attempt: &str,
        published: &PublicationEvidence,
        assembly: &AssemblyEvidence,
    ) -> Result<(), AppError> {
        live(self.deadline, &self.cancellation)?;
        validate_binding(&self.evidence, owner, attempt, published, assembly)
    }

    /// One consumption, without renewing the observation's original lifetime.
    /// No nested Store read: the caller owns the phase/admission transaction.
    pub fn consume_for(
        self,
        claim: &IntegrationGateClaim,
    ) -> Result<IntegrationGateInputsEvidence, AppError> {
        self.validate_for(claim)?;
        Ok(self.evidence)
    }
}

// Owner tests replace only remote observation; native assembly/store custody
// remains real. No production construction or persisted-JSON adoption exists.
#[cfg(test)]
pub(super) fn fixture_gate_inputs(
    evidence: IntegrationGateInputsEvidence,
    deadline: Instant,
    cancellation: Cancellation,
) -> NativeIntegrationGateInputs {
    NativeIntegrationGateInputs {
        evidence,
        deadline,
        cancellation,
    }
}

/// Refresh original/managed metadata, current remote refs and private objects
/// under one independently bounded observation operation. Preserve the central
/// project guard through quiescent child drain; this function never releases it.
pub fn observe_gate_inputs<S: Store>(
    service: &IntegrationOwnerService<'_, S>,
    claim: &IntegrationGateClaim,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<NativeIntegrationGateInputs, AppError> {
    live(deadline, cancellation)?;
    if !service.gate_permitted(claim)? {
        return Err(refuse("gate permission was revoked"));
    }
    let cancelled =
        || cancellation.is_cancelled() || !matches!(service.gate_permitted(claim), Ok(true));
    let original = &claim.publication().original;
    let repository = Repository::resolve_publication(
        &original.checkout,
        service.environment(),
        &original.repository,
        deadline,
        &cancelled,
    )?;
    let reader = NativeReader {
        repository,
        assembly: claim.assembly(),
        deadline,
        cancelled: &cancelled,
    };
    let evidence = read_inputs(
        claim.id(),
        claim.attempt(),
        claim.publication(),
        claim.assembly(),
        &reader,
    )?;
    live(deadline, cancellation)?;
    if !service.gate_permitted(claim)? {
        return Err(refuse("gate permission changed during observation"));
    }
    let native = NativeIntegrationGateInputs {
        evidence,
        deadline,
        cancellation: cancellation.clone(),
    };
    native.validate_for(claim)?;
    Ok(native)
}

// Private boundary makes refusal sequencing testable without manufacturing
// native claim custody, making a network call or certifying fixture JSON.
trait Reader {
    fn pull_request(&self, number: u64) -> Result<Vec<u8>, AppError>;
    fn private_objects(&self) -> Result<(), AppError>;
    fn ancestor(&self, ancestor: &str, descendant: &str) -> Result<(), AppError>;
    fn remote_head(&self, branch: &str) -> Result<Option<String>, AppError>;
}
struct NativeReader<'a> {
    repository: Repository,
    assembly: &'a AssemblyEvidence,
    deadline: Instant,
    cancelled: &'a dyn Fn() -> bool,
}
impl Reader for NativeReader<'_> {
    fn pull_request(&self, number: u64) -> Result<Vec<u8>, AppError> {
        self.repository.gh_publication(
            &strings(&[
                "api",
                &publication::endpoint(&self.repository, &format!("pulls/{number}")),
                "--jq",
                publication::PR_FIELDS,
            ]),
            self.deadline,
            self.cancelled,
        )
    }
    fn private_objects(&self) -> Result<(), AppError> {
        publication::verify_objects(self.assembly, self.deadline, self.cancelled)
    }
    fn ancestor(&self, ancestor: &str, descendant: &str) -> Result<(), AppError> {
        publication::private_git(
            self.assembly,
            &["merge-base", "--is-ancestor", ancestor, descendant],
            self.deadline,
            self.cancelled,
        )
        .map(|_| ())
    }
    fn remote_head(&self, branch: &str) -> Result<Option<String>, AppError> {
        let reference = format!("refs/heads/{branch}");
        let bytes = self.repository.git_publication(
            &strings(&["ls-remote", "--heads", "origin", &reference]),
            None,
            self.deadline,
            self.cancelled,
        )?;
        publication::parse_remote_head(&bytes, &reference)
    }
}

fn read_inputs(
    owner: &str,
    attempt: &str,
    published: &PublicationEvidence,
    assembly: &AssemblyEvidence,
    reader: &impl Reader,
) -> Result<IntegrationGateInputsEvidence, AppError> {
    let evidence = IntegrationGateInputsEvidence {
        version: 1,
        owner: owner.into(),
        attempt: attempt.into(),
        publication: published.clone(),
        current_base: published.original.base.clone(),
        base_branch: published.original.base_branch.clone(),
        tree: assembly.tree.clone(),
        policy: assembly.plan.policy.clone(),
        parents: [assembly.plan.base.clone(), assembly.plan.head.clone()],
    };
    validate_binding(&evidence, owner, attempt, published, assembly)?;
    let original = crate::domain::pr_url::parse_pr_url(&published.original.pull_request)?;
    publication::validate_original(
        &publication::decode_pr(&reader.pull_request(original.number)?)?,
        &published.original,
    )?;
    let managed = publication::decode_pr(&reader.pull_request(published.number)?)?;
    publication::validate_managed(&managed, &published.original, assembly, &published.marker)?;
    if managed.number != published.number
        || crate::domain::pr_url::parse_pr_url(&managed.html_url)?
            != crate::domain::pr_url::parse_pr_url(&published.pull_request)?
    {
        return Err(refuse("managed PR differs from exact retained publication"));
    }
    reader.private_objects()?;
    reader.ancestor(&assembly.plan.base, &assembly.commit)?;
    reader.ancestor(&assembly.plan.head, &assembly.commit)?;
    // This initial implementation cannot silently certify against a new base.
    // Exact fresh remote refs are required even when PR metadata looks stable.
    if reader.remote_head(&published.branch)?.as_deref() != Some(published.commit.as_str()) {
        return Err(refuse("managed remote branch moved or disappeared"));
    }
    if reader
        .remote_head(&published.original.base_branch)?
        .as_deref()
        != Some(published.original.base.as_str())
    {
        return Err(refuse(
            "current base differs from pinned base; reinspection required",
        ));
    }
    Ok(evidence)
}

fn validate_binding(
    evidence: &IntegrationGateInputsEvidence,
    owner: &str,
    attempt: &str,
    published: &PublicationEvidence,
    assembly: &AssemblyEvidence,
) -> Result<(), AppError> {
    if evidence.version != 1
        || evidence.owner != owner
        || evidence.attempt != attempt
        || evidence.publication != *published
        || published.owner != owner
        || assembly.owner != owner
        || published.original != assembly.submission
        || published.branch != assembly.branch
        || published.commit != assembly.commit
        || published.tree != assembly.tree
        || published.parents != [assembly.plan.base.clone(), assembly.plan.head.clone()]
        || evidence.current_base != assembly.plan.base
        || evidence.current_base != published.original.base
        || evidence.base_branch != published.original.base_branch
        || evidence.tree != assembly.tree
        || evidence.policy != assembly.plan.policy
        || evidence.parents != published.parents
        || published.original.head != assembly.plan.head
    {
        return Err(refuse(
            "gate input observation differs from exact owner, attempt or native assembly",
        ));
    }
    Ok(())
}
fn live(deadline: Instant, cancellation: &Cancellation) -> Result<(), AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        Err(refuse(
            "original gate input observation expired or was cancelled",
        ))
    } else {
        Ok(())
    }
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}
fn refuse(detail: &str) -> AppError {
    AppError::Validation(format!("managed gate inputs held: {detail}"))
}

#[cfg(test)]
mod tests;
