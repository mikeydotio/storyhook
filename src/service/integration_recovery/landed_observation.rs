//! Native landed facts from new private custody, never adoption of an old lane.
use super::{
    IntegrationLandingObservation, IntegrationOwnerService, PublicationEvidence, publication,
};
use crate::{
    error::AppError,
    github_access::{
        Repository,
        private_fetch::{PrivateFetch, full_oid},
    },
    process::Cancellation,
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Instant};

/// Observational only. Persisted JSON cannot recreate native completion proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationLandedEvidence {
    pub version: u8,
    pub owner: String,
    pub intent_id: String,
    pub repository: String,
    pub original_pr: String,
    pub original_head: String,
    pub managed_pr: String,
    pub managed_head: String,
    pub merge_commit: String,
    pub merge_tree: String,
    pub base_branch: String,
    pub observed_base: String,
    pub observed_base_tree: String,
}

/// Opaque fresh proof, not Clone/Deserialize or merge authority. Dropping it
/// retains resources; the controller must explicitly settle after its CAS.
pub struct NativeIntegrationLanded {
    query: IntegrationLandingObservation,
    evidence: IntegrationLandedEvidence,
    private: PrivateFetch,
    deadline: Instant,
    cancellation: Cancellation,
}
impl NativeIntegrationLanded {
    pub fn query(&self) -> &IntegrationLandingObservation {
        &self.query
    }
    pub fn evidence(&self) -> &IntegrationLandedEvidence {
        &self.evidence
    }
    /// Current new private resource identity, for an honest cleanup receipt.
    pub fn observation_path(&self) -> &Path {
        self.private.path()
    }
    /// No nested Store read. Main must separately recheck durable owner/human
    /// generation inside the same transaction which accepts this observation.
    pub fn validate_lifetime(&self) -> Result<(), AppError> {
        self.query.validate_lifetime()?;
        live(self.deadline, &self.cancellation)?;
        self.private.validate()?;
        finish_lifetime(self.deadline, &self.cancellation, || {
            self.query.validate_lifetime()
        })
    }
    /// Explicit cleanup of only this newly created observation repository. Does
    /// not require an unexpired query: after expiry cleanup still belongs here.
    pub fn settle(self) -> Result<(), AppError> {
        self.private.settle()
    }
}

/// Observe once under independent bounded control lifetime. The previous merge
/// operation is never repeated, renewed or inferred from its durable intent.
pub fn observe_landed_owned<S: Store>(
    service: &IntegrationOwnerService<'_, S>,
    query: IntegrationLandingObservation,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<NativeIntegrationLanded, AppError> {
    query.validate_lifetime()?;
    live(deadline, cancellation)?;
    if !service.landing_observation_permitted(&query)? {
        return Err(refuse("landing query no longer retained"));
    }
    let cancelled = || {
        cancellation.is_cancelled()
            || query.validate_lifetime().is_err()
            || !matches!(service.landing_observation_permitted(&query), Ok(true))
    };
    let original = &query.publication().original;
    let repository = Repository::resolve_publication(
        &original.checkout,
        service.environment(),
        &original.repository,
        deadline,
        &cancelled,
    )?;
    let private = PrivateFetch::create(&repository, deadline, &cancelled)?;
    let result = (|| {
        let guarded = || cancelled() || private.validate_live_custody().is_err();
        let reader = NativeReader {
            repository,
            private: &private,
            deadline,
            cancelled: &guarded,
        };
        let certificate = &query.certification().certification;
        if certificate.head != query.publication().commit
            || certificate.tree != query.publication().tree
        {
            return Err(refuse(
                "retained certificate differs from managed publication",
            ));
        }
        let evidence = read_landed(
            query.id(),
            &query.intent().id,
            query.publication(),
            &certificate.tree,
            &reader,
        )?;
        query.validate_lifetime()?;
        live(deadline, cancellation)?;
        if !service.landing_observation_permitted(&query)? {
            return Err(refuse("landing query changed while observing"));
        }
        private.validate()?;
        finish_lifetime(deadline, cancellation, || query.validate_lifetime())?;
        Ok(evidence)
    })();
    let evidence = result.map_err(|error| private.residue(error))?;
    Ok(NativeIntegrationLanded {
        query,
        evidence,
        private,
        deadline,
        cancellation: cancellation.clone(),
    })
}

trait Reader {
    fn pr(&self, number: u64) -> Result<Vec<u8>, AppError>;
    fn base(&self, branch: &str) -> Result<String, AppError>;
    fn fetch(&self, oids: &[&str]) -> Result<(), AppError>;
    fn git(&self, arguments: &[&str]) -> Result<Vec<u8>, AppError>;
}
struct NativeReader<'a> {
    repository: Repository,
    private: &'a PrivateFetch,
    deadline: Instant,
    cancelled: &'a dyn Fn() -> bool,
}
impl Reader for NativeReader<'_> {
    fn pr(&self, number: u64) -> Result<Vec<u8>, AppError> {
        self.repository.gh_publication(
            &strings(&[
                "api",
                &publication::endpoint(&self.repository, &format!("pulls/{number}")),
                "--jq",
                PR_FIELDS,
            ]),
            self.deadline,
            self.cancelled,
        )
    }
    fn base(&self, branch: &str) -> Result<String, AppError> {
        let reference = format!("refs/heads/{branch}");
        let bytes = self.repository.git_publication(
            &strings(&["ls-remote", "--heads", "origin", &reference]),
            None,
            self.deadline,
            self.cancelled,
        )?;
        publication::parse_remote_head(&bytes, &reference)?
            .ok_or_else(|| refuse("target branch disappeared"))
    }
    fn fetch(&self, oids: &[&str]) -> Result<(), AppError> {
        self.private
            .fetch(&self.repository, oids, self.deadline, self.cancelled)
    }
    fn git(&self, arguments: &[&str]) -> Result<Vec<u8>, AppError> {
        self.private.read(arguments, self.deadline, self.cancelled)
    }
}
const PR_FIELDS: &str = "{number,html_url,state,merged,merge_commit_sha,body,base:{sha:.base.sha,ref:.base.ref,repository:.base.repo.full_name},head:{sha:.head.sha,ref:.head.ref,repository:.head.repo.full_name}}";
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct PullRequest {
    number: u64,
    html_url: String,
    state: String,
    merged: bool,
    merge_commit_sha: Option<String>,
    body: Option<String>,
    base: Side,
    head: Side,
}
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Side {
    sha: String,
    #[serde(rename = "ref")]
    branch: String,
    repository: String,
}
fn decode(bytes: &[u8]) -> Result<PullRequest, AppError> {
    serde_json::from_slice(bytes).map_err(|_| refuse("invalid exact managed PR answer"))
}
fn managed(pr: &PullRequest, published: &PublicationEvidence) -> Result<String, AppError> {
    let parsed = crate::domain::pr_url::parse_pr_url(&published.pull_request)?;
    let repository = format!("{}/{}", parsed.owner, parsed.repo);
    if pr.number != published.number
        || crate::domain::pr_url::parse_pr_url(&pr.html_url)? != parsed
        || pr.state != "closed"
        || !pr.merged
        || pr.head.sha != published.commit
        || pr.head.branch != published.branch
        || pr.head.repository != repository
        || pr.base.repository != repository
        || pr.base.branch != published.original.base_branch
        || !pr
            .body
            .as_deref()
            .is_some_and(|body| body.lines().any(|line| line == published.marker))
    {
        return Err(refuse(
            "managed PR is not the exact retained merged publication",
        ));
    }
    pr.merge_commit_sha
        .as_ref()
        .filter(|oid| full_oid(oid))
        .cloned()
        .ok_or_else(|| refuse("merged PR lacks full actual merge object"))
}
fn read_landed(
    owner: &str,
    intent: &str,
    published: &PublicationEvidence,
    tree: &str,
    reader: &impl Reader,
) -> Result<IntegrationLandedEvidence, AppError> {
    if published.owner != owner || published.tree != tree || !full_oid(tree) {
        return Err(refuse(
            "landed query differs from retained owner/certification",
        ));
    }
    let pr = decode(&reader.pr(published.number)?)?;
    let merge = managed(&pr, published)?;
    let base = reader.base(&published.original.base_branch)?;
    if !full_oid(&base) {
        return Err(refuse("invalid current target object"));
    }
    reader.fetch(&[&published.commit, &merge, &base])?;
    // Complete closure must exist without source alternates/lazy fetch. Prove
    // all objects even when MERGED metadata would otherwise look sufficient.
    reader.git(&[
        "fsck",
        "--full",
        "--no-reflogs",
        "--no-dangling",
        &published.commit,
        &merge,
        &base,
    ])?;
    let merge_tree =
        object(reader.git(&["rev-parse", "--verify", &format!("{merge}^{{tree}}")])?)?;
    if merge_tree != tree {
        return Err(refuse("actual merged tree differs from certified tree"));
    }
    for ancestor in [&published.commit, &published.original.head] {
        reader.git(&["merge-base", "--is-ancestor", ancestor, &merge])?;
    }
    reader.git(&["merge-base", "--is-ancestor", &merge, &base])?;
    let base_tree = object(reader.git(&["rev-parse", "--verify", &format!("{base}^{{tree}}")])?)?;
    if decode(&reader.pr(published.number)?)? != pr
        || reader.base(&published.original.base_branch)? != base
    {
        return Err(refuse(
            "managed merge or actual target changed during proof",
        ));
    }
    Ok(IntegrationLandedEvidence {
        version: 1,
        owner: owner.into(),
        intent_id: intent.into(),
        repository: published.original.repository.clone(),
        original_pr: published.original.pull_request.clone(),
        original_head: published.original.head.clone(),
        managed_pr: published.pull_request.clone(),
        managed_head: published.commit.clone(),
        merge_commit: merge,
        merge_tree,
        base_branch: published.original.base_branch.clone(),
        observed_base: base,
        observed_base_tree: base_tree,
    })
}
fn object(bytes: Vec<u8>) -> Result<String, AppError> {
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| refuse("native object ID is not UTF-8"))?
        .trim();
    if !full_oid(value) {
        return Err(refuse("native object ID is not exact"));
    }
    Ok(value.into())
}
fn finish_lifetime(
    deadline: Instant,
    cancellation: &Cancellation,
    custody: impl FnOnce() -> Result<(), AppError>,
) -> Result<(), AppError> {
    custody()?;
    live(deadline, cancellation)
}
fn live(deadline: Instant, cancellation: &Cancellation) -> Result<(), AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        Err(refuse("original landed observation lifetime ended"))
    } else {
        Ok(())
    }
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}
fn refuse(detail: &str) -> AppError {
    AppError::Validation(format!("managed landed observation held: {detail}"))
}
#[cfg(test)]
mod tests;
