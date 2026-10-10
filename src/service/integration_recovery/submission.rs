//! Bind a native proposal to the live origin-bound original PR, never public JSON.
use super::*;
use crate::{
    domain::pr_url::parse_pr_url, github_access::Repository, service::VerificationCandidate,
};

mod clean;
pub use clean::{CleanIntegrationEvidence, NativeCleanIntegration, observe_clean_submission};

/// Retained native PR metadata. Deserializing this evidence grants no effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionObservation {
    /// Canonical registered checkout used by native queries.
    pub checkout: std::path::PathBuf,
    /// Fully qualified origin, including host and optional port.
    pub repository: String,
    /// Original PR identity returned by the same origin.
    pub pull_request: String,
    /// Current target branch, never selected from caller advice.
    pub base_branch: String,
    /// Exact current target commit.
    pub base: String,
    /// Original author's branch commit, never rewritten by integration recovery.
    pub head: String,
}

/// Native submission inspection bound to live PR metadata and private objects.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::BoundIntegrationProposal;
/// let _: BoundIntegrationProposal = serde_json::from_str("{}").unwrap();
/// ```
pub struct BoundIntegrationProposal {
    pub(super) proposal: Box<IntegrationProposal>,
    pub(super) submission: SubmissionObservation,
    pub(super) deadline: Instant,
    pub(super) cancellation: Cancellation,
}

impl BoundIntegrationProposal {
    /// An observation cannot outlive its original bounded native operation.
    pub(super) fn check_live(&self) -> Result<(), AppError> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(AppError::Validation(
                "native integration proposal expired or its owner cancelled".into(),
            ));
        }
        Ok(())
    }

    /// Exact source evidence to retain before a managed external effect.
    #[must_use]
    pub fn plan(&self) -> &IntegrationPlan {
        self.proposal.plan()
    }
    /// Live origin-bound submitted identity.
    #[must_use]
    pub fn submission(&self) -> &SubmissionObservation {
        &self.submission
    }
    /// Explicitly settle this read-only native inspection.
    pub fn settle(self) -> Result<(IntegrationPlan, SubmissionObservation), AppError> {
        Ok((self.proposal.settle()?, self.submission))
    }
}

/// A live PR read plus native merge inspection; none grants certification.
pub enum BoundInspection {
    /// Ordinary merge needs no integration owner.
    Clean,
    /// Original submission stays held on this concrete constraint.
    Held(String),
    /// Capability to reserve a distinct integration owner for this submission.
    Proposed(BoundIntegrationProposal),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    number: u64,
    html_url: String,
    state: String,
    merged: bool,
    base: Side,
    head: Side,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Side {
    sha: String,
    #[serde(rename = "ref")]
    branch: String,
}

pub(super) fn read_submission(
    candidate: &VerificationCandidate,
    environment: &crate::env::Environment,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<SubmissionObservation, AppError> {
    let original = candidate.pull_request.as_ref().map_err(|_| {
        AppError::Validation("integration requires the original submitted PR".into())
    })?;
    let expected = parse_pr_url(&original.url)?;
    let cancelled = || cancellation.is_cancelled();
    let repository = Repository::resolve_publication(
        &candidate.checkout,
        environment,
        &format!("{}/{}/{}", expected.host, expected.owner, expected.repo),
        deadline,
        &cancelled,
    )?;
    let identity = repository.identity();
    if !expected.host.eq_ignore_ascii_case(&identity.host)
        || !expected.owner.eq_ignore_ascii_case(&identity.owner)
        || !expected.repo.eq_ignore_ascii_case(&identity.repo)
    {
        return Err(AppError::Validation(
            "integration PR differs from current registered origin".into(),
        ));
    }
    let raw = repository.gh_publication(&[
        "api".into(), format!("repos/{}/{}/pulls/{}", identity.owner, identity.repo, expected.number),
        "--jq".into(), "{number,html_url,state,merged,base:{sha:.base.sha,ref:.base.ref},head:{sha:.head.sha,ref:.head.ref}}".into(),
    ], deadline, &cancelled)?;
    let metadata: Metadata = serde_json::from_slice(&raw)
        .map_err(|e| AppError::GithubApi(format!("invalid native integration PR metadata: {e}")))?;
    validate_metadata(&metadata, &expected)?;
    Ok(SubmissionObservation {
        checkout: candidate
            .checkout
            .canonicalize()
            .map_err(|e| AppError::Validation(format!("integration checkout unavailable: {e}")))?,
        repository: repository.qualified(),
        pull_request: metadata.html_url,
        base_branch: metadata.base.branch,
        base: metadata.base.sha,
        head: metadata.head.sha,
    })
}

fn validate_metadata(
    metadata: &Metadata,
    expected: &crate::domain::pr_url::PullRequestRef,
) -> Result<(), AppError> {
    let observed = parse_pr_url(&metadata.html_url)?;
    if observed.number != expected.number
        || metadata.number != expected.number
        || !observed.host.eq_ignore_ascii_case(&expected.host)
        || !observed.owner.eq_ignore_ascii_case(&expected.owner)
        || !observed.repo.eq_ignore_ascii_case(&expected.repo)
        || metadata.state != "open"
        || metadata.merged
        || [&metadata.base.sha, &metadata.head.sha]
            .iter()
            .any(|oid| !crate::service::project_fault::is_pinned_oid(oid))
        || [&metadata.base.branch, &metadata.head.branch]
            .iter()
            .any(|branch| {
                branch.is_empty() || branch.starts_with('-') || branch.chars().any(char::is_control)
            })
    {
        return Err(AppError::Validation(
            "native integration PR identity, open state, or pinned inputs changed".into(),
        ));
    }
    Ok(())
}

/// Reobserve the exact submitted PR at one bounded native boundary, then inspect
/// its immutable head and current base using only the base's explicit policy.
pub fn inspect_submission(
    candidate: &VerificationCandidate,
    retained_head: &str,
    environment: &crate::env::Environment,
    deadline: Instant,
    cancellation: Cancellation,
) -> Result<BoundInspection, AppError> {
    require_pinned(retained_head, "retained integration submission")?;
    let submission = read_submission(candidate, environment, deadline, &cancellation)?;
    if submission.head != retained_head {
        return Ok(BoundInspection::Held(
            "the original submitted head changed; integration cannot adopt its replacement".into(),
        ));
    }
    match inspect(
        &submission.checkout,
        &submission.base,
        &submission.head,
        deadline,
        cancellation.clone(),
    )? {
        Inspection::Clean { .. } => Ok(BoundInspection::Clean),
        Inspection::Held { reason } => Ok(BoundInspection::Held(reason)),
        Inspection::Proposed(proposal) => Ok(BoundInspection::Proposed(BoundIntegrationProposal {
            proposal,
            submission,
            deadline,
            cancellation,
        })),
    }
}

#[cfg(test)]
pub(super) use clean::observe_clean_for_fixture;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integration_metadata_refuses_host_port_pr_head_and_state_substitutions() {
        let expected = parse_pr_url("https://git.example.test:8443/acme/widgets/pull/7").unwrap();
        let raw = serde_json::json!({"number":7,"html_url":"https://git.example.test:8443/acme/widgets/pull/7","state":"open","merged":false,"base":{"sha":"a".repeat(40),"ref":"dev"},"head":{"sha":"b".repeat(40),"ref":"author"}});
        validate_metadata(&serde_json::from_value(raw.clone()).unwrap(), &expected).unwrap();
        for (field, value) in [
            (
                "html_url",
                serde_json::json!("https://git.example.test/acme/widgets/pull/7"),
            ),
            (
                "html_url",
                serde_json::json!("https://other.example.test:8443/acme/widgets/pull/7"),
            ),
            ("number", serde_json::json!(8)),
            ("state", serde_json::json!("closed")),
            ("merged", serde_json::json!(true)),
            (
                "head",
                serde_json::json!({"sha":"mutable-branch", "ref":"author"}),
            ),
        ] {
            let mut changed = raw.clone();
            changed[field] = value;
            assert!(
                validate_metadata(&serde_json::from_value(changed).unwrap(), &expected).is_err()
            );
        }
    }
}
