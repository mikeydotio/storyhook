//! Immutable evidence supplied by verification before a merge is authorized.

use serde::{Deserialize, Serialize};

/// Observed Git ancestry, independent of release-gate certification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlreadyLanded {
    /// Origin identity as host/owner/repository.
    pub repository: String,
    /// Full commit id of the submitted branch or merged PR head.
    pub head_oid: String,
    /// Remote default branch observed by this operation.
    pub base: String,
    /// Fetched base commit containing the head.
    pub base_oid: String,
    /// Exact tree of the containing base commit.
    pub base_tree: String,
    /// Linked PR positively observed merged, if any; ancestry alone supplies none.
    #[serde(default)]
    pub merged_pr: Option<String>,
}

impl AlreadyLanded {
    /// Rejects incomplete evidence before it can complete a story.
    pub fn validate(&self) -> Result<(), crate::error::AppError> {
        use crate::error::AppError;
        for (name, oid) in [
            ("head", &self.head_oid),
            ("base", &self.base_oid),
            ("tree", &self.base_tree),
        ] {
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(AppError::Validation(format!(
                    "landed {name} must be a full Git object id"
                )));
            }
        }
        let identity =
            super::github_remote::parse_github_url(&format!("https://{}", self.repository))
                .ok_or_else(|| {
                    AppError::Validation("landed evidence has no valid repository identity".into())
                })?;
        if self.base.trim().is_empty() {
            return Err(AppError::Validation(
                "landed evidence has no base branch".into(),
            ));
        }
        if let Some(url) = &self.merged_pr {
            let pr = super::pr_url::parse_pr_url(url)?;
            if !identity.host.eq_ignore_ascii_case(&pr.host)
                || !identity.owner.eq_ignore_ascii_case(&pr.owner)
                || !identity.repo.eq_ignore_ascii_case(&pr.repo)
            {
                return Err(AppError::Validation(
                    "landed PR belongs to another repository".into(),
                ));
            }
        }
        Ok(())
    }
}

/// A successful submission either published an open PR or proved the work landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmissionOutcome {
    /// A branch still needing verification and landing.
    PullRequest(super::SubmittedPullRequest),
    /// Work already contained by the remote default branch.
    AlreadyLanded(AlreadyLanded),
}

impl From<super::SubmittedPullRequest> for SubmissionOutcome {
    fn from(value: super::SubmittedPullRequest) -> Self {
        Self::PullRequest(value)
    }
}

/// The exact Git objects and gate certified by a verification attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedSubmission {
    /// Pull request head that was tested.
    pub head: String,
    /// Certified proposed merge tree.
    pub tree: String,
    /// Human-readable gate command used for the certification.
    pub gate: String,
}

impl VerifiedSubmission {
    /// Rejects incomplete certification before it can authorize a process argument.
    pub fn validate(&self) -> Result<(), crate::error::AppError> {
        for (name, oid) in [("head", &self.head), ("tree", &self.tree)] {
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(crate::error::AppError::Validation(format!(
                    "verified {name} must be a full Git object id"
                )));
            }
        }
        if self.gate.trim().is_empty() {
            return Err(crate::error::AppError::Validation(
                "verification certification has no gate".into(),
            ));
        }
        Ok(())
    }
}
