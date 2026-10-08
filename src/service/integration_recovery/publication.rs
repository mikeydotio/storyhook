//! One native publication attempt, never replay authority or landing evidence.
//!
//! The caller retains its central worker slot through all quiescent captures.
//! Failure leaves durable effect intent and private custody with that owner.
//! Normal protected Git hooks/configuration may refuse publication; their
//! refusal is not a reason to change identity, signatures or repository policy.
use super::{
    AssemblyEvidence, BoundIntegrationProposal, IntegrationOwnerService, PublicationClaim,
    PublicationEffect, SubmissionObservation, policy_from_pointer,
};
use crate::{
    env::git_env,
    error::AppError,
    github_access::Repository,
    process::{Cancellation, run_captured_query_quiescent},
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::{process::Command, time::Instant};

/// Observable publication identity. JSON cannot acquire publication authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationEvidence {
    /// Evidence format.
    pub version: u8,
    /// Durable owner and its publication epoch.
    pub owner: String,
    pub epoch: u32,
    /// Immutable original submission; it is not marked merged by publication.
    pub original: SubmissionObservation,
    /// Reserved branch and exact native assembly objects.
    pub branch: String,
    pub commit: String,
    pub tree: String,
    pub parents: [String; 2],
    /// Exact owner marker in the separately observed managed PR.
    pub marker: String,
    pub pull_request: String,
    pub number: u64,
}

/// Only this adapter can construct successful native publication evidence.
/// Not Clone or Deserialize, and not a certificate or landing receipt.
pub struct NativePublication {
    evidence: PublicationEvidence,
}
impl NativePublication {
    pub fn evidence(&self) -> &PublicationEvidence {
        &self.evidence
    }
}

// Only unit tests substitute the native remote boundary. Production callers
// cannot reconstruct successful publication from persisted or public JSON.
#[cfg(test)]
pub(super) fn fixture_publication(evidence: PublicationEvidence) -> NativePublication {
    NativePublication { evidence }
}

/// Publish a distinct managed branch and PR once, using the original lifetime.
/// A consumed intent is never treated as proof that an effect succeeded.
pub fn publish_owned<S: Store>(
    service: &IntegrationOwnerService<'_, S>,
    claim: &mut PublicationClaim,
    proof: &BoundIntegrationProposal,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<NativePublication, AppError> {
    if !claim.effects().is_empty() {
        return Err(refuse(
            "publication already has an effect intent; reconcile without replay",
        ));
    }
    check(service, claim, proof, deadline, cancellation)?;
    let assembly = claim.assembly().clone();
    let marker = format!(
        "<!-- storyhook-integration-owner:{}:{}:{} -->",
        claim.id(),
        claim.epoch(),
        assembly.stamp_sha256
    );
    let original = observe(service, claim, proof, deadline, cancellation)?;
    let remote = remote_head(service, claim, proof, deadline, cancellation)?;
    if remote.is_some() {
        return Err(refuse(
            "managed remote branch already exists; reconcile ownership",
        ));
    }
    if !service.claim_publication_effect(claim, proof, PublicationEffect::PushBranch)? {
        return Err(refuse("push intent was not acquired"));
    }
    {
        let cancelled = || {
            cancellation.is_cancelled()
                || !matches!(service.publication_permitted(claim, proof), Ok(true))
        };
        let repo = Repository::resolve_publication(
            &original.checkout,
            service.environment(),
            &claim.submission().repository,
            deadline,
            &cancelled,
        )?;
        claim.validate_custody()?;
        repo.git_publication(
            &strings(&[
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--recurse-submodules=no",
                "origin",
                &format!("{}:refs/heads/{}", assembly.commit, assembly.branch),
            ]),
            Some(&assembly.workspace.path.join("objects")),
            deadline,
            &cancelled,
        )?;
    }
    // Intent plus an exit status is insufficient. Both submission and exact
    // remote branch must be observed again before the independent PR effect.
    observe(service, claim, proof, deadline, cancellation)?;
    if remote_head(service, claim, proof, deadline, cancellation)?.as_deref()
        != Some(&assembly.commit)
    {
        return Err(refuse("push lacks fresh exact remote branch proof"));
    }
    if !service.claim_publication_effect(claim, proof, PublicationEffect::CreatePullRequest)? {
        return Err(refuse("PR creation intent was not acquired"));
    }
    let created = {
        let cancelled = || {
            cancellation.is_cancelled()
                || !matches!(service.publication_permitted(claim, proof), Ok(true))
        };
        let repo = Repository::resolve_publication(
            &original.checkout,
            service.environment(),
            &claim.submission().repository,
            deadline,
            &cancelled,
        )?;
        let endpoint = endpoint(&repo, "pulls");
        let bytes = repo.gh_publication(&strings(&["api", &endpoint, "--method", "POST", "--raw-field", &format!("title=Managed integration {}", claim.id()), "--raw-field", &format!("head={}", assembly.branch), "--raw-field", &format!("base={}", original.base_branch), "--raw-field", &format!("body={marker}\n\nManaged integration of {} at {}. This PR requires independent certification and landing.", original.pull_request, original.head), "--jq", PR_FIELDS]), deadline, &cancelled)?;
        decode_pr(&bytes)?
    };
    validate_managed(&created, &original, &assembly, &marker)?;
    let observed = {
        let cancelled = || {
            cancellation.is_cancelled()
                || !matches!(service.publication_permitted(claim, proof), Ok(true))
        };
        let repo = Repository::resolve_publication(
            &original.checkout,
            service.environment(),
            &claim.submission().repository,
            deadline,
            &cancelled,
        )?;
        let bytes = repo.gh_publication(
            &strings(&[
                "api",
                &endpoint(&repo, &format!("pulls/{}", created.number)),
                "--jq",
                PR_FIELDS,
            ]),
            deadline,
            &cancelled,
        )?;
        decode_pr(&bytes)?
    };
    validate_managed(&observed, &original, &assembly, &marker)?;
    if observed.number != created.number || observed.html_url != created.html_url {
        return Err(refuse("created PR identity changed on observation"));
    }
    observe(service, claim, proof, deadline, cancellation)?;
    if remote_head(service, claim, proof, deadline, cancellation)?.as_deref()
        != Some(&assembly.commit)
    {
        return Err(refuse("managed branch moved after PR creation"));
    }
    check(service, claim, proof, deadline, cancellation)?;
    Ok(NativePublication {
        evidence: PublicationEvidence {
            version: 1,
            owner: claim.id().into(),
            epoch: claim.epoch(),
            original,
            branch: assembly.branch,
            commit: assembly.commit,
            tree: assembly.tree,
            parents: [assembly.plan.base, assembly.plan.head],
            marker,
            pull_request: observed.html_url,
            number: observed.number,
        },
    })
}

fn check<S: Store>(
    service: &IntegrationOwnerService<'_, S>,
    claim: &PublicationClaim,
    proof: &BoundIntegrationProposal,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(), AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(refuse("publication lifetime ended"));
    }
    claim.validate_custody()?;
    if claim.assembly().plan != *proof.plan()
        || claim.submission() != proof.submission()
        || !service.publication_permitted(claim, proof)?
    {
        return Err(refuse("publication authority or pinned proposal changed"));
    }
    Ok(())
}

fn observe<S: Store>(
    service: &IntegrationOwnerService<'_, S>,
    claim: &PublicationClaim,
    proof: &BoundIntegrationProposal,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<SubmissionObservation, AppError> {
    check(service, claim, proof, deadline, cancellation)?;
    let cancelled = || {
        cancellation.is_cancelled()
            || !matches!(service.publication_permitted(claim, proof), Ok(true))
    };
    let expected = claim.submission();
    let repo = Repository::resolve_publication(
        &expected.checkout,
        service.environment(),
        &claim.submission().repository,
        deadline,
        &cancelled,
    )?;
    if repo.qualified() != expected.repository {
        return Err(refuse("registered repository identity changed"));
    }
    let original = crate::domain::pr_url::parse_pr_url(&expected.pull_request)?;
    let bytes = repo.gh_publication(
        &strings(&[
            "api",
            &endpoint(&repo, &format!("pulls/{}", original.number)),
            "--jq",
            PR_FIELDS,
        ]),
        deadline,
        &cancelled,
    )?;
    validate_original(&decode_pr(&bytes)?, expected)?;
    verify_objects(claim.assembly(), deadline, &cancelled)?;
    let mut format = git_env::command(&expected.checkout);
    format.args(["rev-parse", "--show-object-format"]);
    let source_format = capture(format, deadline, &cancelled)?;
    let private_format = private_git(
        claim.assembly(),
        &["rev-parse", "--show-object-format"],
        deadline,
        &cancelled,
    )?;
    if source_format != private_format {
        return Err(refuse("source and private object formats differ"));
    }
    check(service, claim, proof, deadline, cancellation)?;
    Ok(expected.clone())
}

fn remote_head<S: Store>(
    service: &IntegrationOwnerService<'_, S>,
    claim: &PublicationClaim,
    proof: &BoundIntegrationProposal,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<Option<String>, AppError> {
    check(service, claim, proof, deadline, cancellation)?;
    let cancelled = || {
        cancellation.is_cancelled()
            || !matches!(service.publication_permitted(claim, proof), Ok(true))
    };
    let repo = Repository::resolve_publication(
        &claim.submission().checkout,
        service.environment(),
        &claim.submission().repository,
        deadline,
        &cancelled,
    )?;
    let branch = format!("refs/heads/{}", claim.assembly().branch);
    let bytes = repo.git_publication(
        &strings(&["ls-remote", "--heads", "origin", &branch]),
        None,
        deadline,
        &cancelled,
    )?;
    parse_remote_head(&bytes, &branch)
}
fn parse_remote_head(bytes: &[u8], branch: &str) -> Result<Option<String>, AppError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| refuse("remote branch answer is not UTF-8"))?;
    if text.is_empty() {
        return Ok(None);
    }
    let lines: Vec<_> = text.lines().collect();
    if lines.len() != 1 {
        return Err(refuse("remote branch answer is ambiguous"));
    }
    let (oid, reference) = lines[0]
        .split_once('\t')
        .ok_or_else(|| refuse("malformed remote branch answer"))?;
    if reference != branch || !full_oid(oid) {
        return Err(refuse(
            "remote branch answer differs from exact requested ref",
        ));
    }
    Ok(Some(oid.into()))
}

fn verify_objects(
    assembly: &AssemblyEvidence,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), AppError> {
    if !full_oid(&assembly.commit)
        || !full_oid(&assembly.tree)
        || !full_oid(&assembly.plan.base)
        || !full_oid(&assembly.plan.head)
    {
        return Err(refuse("assembly has malformed object identities"));
    }
    let observed = private_git(
        assembly,
        &["show", "-s", "--format=%T%n%P", &assembly.commit],
        deadline,
        cancelled,
    )?;
    validate_commit(&observed, assembly)?;
    private_git(
        assembly,
        &[
            "fsck",
            "--connectivity-only",
            "--no-reflogs",
            &assembly.commit,
        ],
        deadline,
        cancelled,
    )?;
    let pointer = private_git(
        assembly,
        &["show", &format!("{}:.storyhook.toml", assembly.plan.base)],
        deadline,
        cancelled,
    )?;
    let policy = policy_from_pointer(Some(&pointer)).map_err(|e| refuse(&e))?;
    if !policy.enabled || policy.digest != assembly.plan.policy {
        return Err(refuse("committed base policy changed or is disabled"));
    }
    Ok(())
}
fn validate_commit(bytes: &[u8], assembly: &AssemblyEvidence) -> Result<(), AppError> {
    let expected = format!(
        "{}\n{} {}\n",
        assembly.tree, assembly.plan.base, assembly.plan.head
    );
    if bytes != expected.as_bytes() {
        return Err(refuse("private commit tree or ordered parents changed"));
    }
    Ok(())
}
fn private_git(
    assembly: &AssemblyEvidence,
    args: &[&str],
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, AppError> {
    let root = &assembly.workspace.path;
    let mut command = git_env::command(root);
    // Read-only validation uses the exact isolated namespace retained by the
    // native claim, never source replace refs, grafts, alternates or attributes.
    command
        .env("GIT_DIR", root)
        .env("GIT_COMMON_DIR", root)
        .env("GIT_OBJECT_DIRECTORY", root.join("objects"))
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--bare",
            "--no-replace-objects",
            "-c",
            "protocol.allow=never",
        ])
        .args(args);
    capture(command, deadline, cancelled)
}
fn capture(
    command: Command,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, AppError> {
    let output = run_captured_query_quiescent(command, deadline, cancelled, 64 * 1024, &[])
        .map_err(|e| refuse(&format!("native observation failed: {}", e.detail())))?;
    if !output.status.success() || output.stdout_truncated {
        return Err(refuse("native object observation failed or was truncated"));
    }
    Ok(output.stdout)
}

const PR_FIELDS: &str = "{number,html_url,state,merged,body,base:{sha:.base.sha,ref:.base.ref,repository:.base.repo.full_name},head:{sha:.head.sha,ref:.head.ref,repository:.head.repo.full_name}}";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PullRequest {
    number: u64,
    html_url: String,
    state: String,
    merged: bool,
    body: Option<String>,
    base: Side,
    head: Side,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Side {
    sha: String,
    #[serde(rename = "ref")]
    branch: String,
    repository: String,
}
fn decode_pr(bytes: &[u8]) -> Result<PullRequest, AppError> {
    serde_json::from_slice(bytes).map_err(|_| refuse("PR answer is malformed or incomplete"))
}
fn validate_original(pr: &PullRequest, original: &SubmissionObservation) -> Result<(), AppError> {
    let actual = crate::domain::pr_url::parse_pr_url(&pr.html_url)?;
    let expected = crate::domain::pr_url::parse_pr_url(&original.pull_request)?;
    if actual != expected
        || pr.number != expected.number
        || pr.state != "open"
        || pr.merged
        || pr.base.sha != original.base
        || pr.base.branch != original.base_branch
        || pr.head.sha != original.head
        || !pr
            .base
            .repository
            .eq_ignore_ascii_case(&format!("{}/{}", expected.owner, expected.repo))
    {
        return Err(refuse("original PR identity, state, base or head changed"));
    }
    Ok(())
}
fn validate_managed(
    pr: &PullRequest,
    original: &SubmissionObservation,
    assembly: &AssemblyEvidence,
    marker: &str,
) -> Result<(), AppError> {
    let actual = crate::domain::pr_url::parse_pr_url(&pr.html_url)?;
    let source = crate::domain::pr_url::parse_pr_url(&original.pull_request)?;
    let repository = format!("{}/{}", source.owner, source.repo);
    if actual.host != source.host
        || actual.owner != source.owner
        || actual.repo != source.repo
        || actual.number != pr.number
        || actual.number == source.number
        || pr.state != "open"
        || pr.merged
        || pr.base.sha != original.base
        || pr.base.branch != original.base_branch
        || pr.head.sha != assembly.commit
        || pr.head.branch != assembly.branch
        || !pr.base.repository.eq_ignore_ascii_case(&repository)
        || !pr.head.repository.eq_ignore_ascii_case(&repository)
        || pr
            .body
            .as_deref()
            .is_none_or(|body| body.lines().filter(|line| *line == marker).count() != 1)
    {
        return Err(refuse(
            "managed PR lacks exact distinct owner/repository/base/head identity",
        ));
    }
    Ok(())
}
fn endpoint(repo: &Repository, suffix: &str) -> String {
    format!(
        "repos/{}/{}/{suffix}",
        repo.identity().owner,
        repo.identity().repo
    )
}
fn full_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).into()).collect()
}
fn refuse(message: &str) -> AppError {
    AppError::Validation(format!("managed publication held: {message}"))
}

#[cfg(test)]
mod tests;
