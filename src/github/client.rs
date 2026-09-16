//! Pull-request observations through the shared gh boundary.
use super::types::PullRequestStatus;
use crate::domain::pr_url::parse_pr_url;
use crate::error::AppError;
use crate::github_access::Repository;
use serde::Deserialize;

/// A gh client bound to one validated checkout origin.
pub struct GithubClient {
    repository: Repository,
}

#[derive(Deserialize)]
struct Observation {
    number: u64,
    html_url: String,
    state: String,
    merged: bool,
}

impl GithubClient {
    /// Creates a client without reading or storing a credential.
    pub fn new(repository: Repository) -> Self {
        Self { repository }
    }

    /// Reads a PR and refuses redirects or metadata for a different identity.
    pub fn get_pull_request(&self, number: u64) -> Result<PullRequestStatus, AppError> {
        let identity = self.repository.identity();
        let endpoint = format!("repos/{}/{}/pulls/{number}", identity.owner, identity.repo);
        let bytes = self.repository.gh(&[
            "api".into(),
            endpoint,
            "--jq".into(),
            "{number,html_url,state,merged}".into(),
        ])?;
        let observation: Observation = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::GithubApi(format!(
                "invalid PR response for {} #{number}: {error}",
                self.repository.qualified()
            ))
        })?;
        let reference = parse_pr_url(&observation.html_url)?;
        if reference.number != number
            || observation.number != number
            || !reference.host.eq_ignore_ascii_case(&identity.host)
            || !reference.owner.eq_ignore_ascii_case(&identity.owner)
            || !reference.repo.eq_ignore_ascii_case(&identity.repo)
        {
            return Err(AppError::GithubApi(format!(
                "PR response identity differs from current origin {} #{number}; update origin and relink before retrying",
                self.repository.qualified()
            )));
        }
        if !matches!(observation.state.as_str(), "open" | "closed")
            || (observation.merged && observation.state != "closed")
        {
            return Err(AppError::GithubApi(
                "invalid PR state in gh response".into(),
            ));
        }
        Ok(PullRequestStatus {
            state: observation.state,
            merged: observation.merged,
        })
    }
}
