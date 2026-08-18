use std::time::Duration;

use ureq::Agent;

use crate::error::AppError;

use super::types::PullRequestStatus;

const API_BASE: &str = "https://api.github.com";

/// GitHub REST API client wrapping ureq.
pub struct GithubClient {
    agent: Agent,
    token: String,
    owner: String,
    repo: String,
}

impl GithubClient {
    /// Create a new client for the given repository.
    pub fn new(token: String, owner: String, repo: String) -> Self {
        let config = Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build();

        let agent: Agent = config.into();

        Self {
            agent,
            token,
            owner,
            repo,
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Build a GET request with common headers.
    fn get(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let url = format!("{API_BASE}{path}");
        self.agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "storyhook")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// Check rate limit headers and warn when remaining calls are low.
    fn check_rate_limit(
        &self,
        response: &ureq::http::Response<ureq::Body>,
    ) -> Result<(), AppError> {
        const LOW_RATE_LIMIT_THRESHOLD: u64 = 50;
        if let Some(remaining) = response.headers().get("x-ratelimit-remaining")
            && let Ok(s) = remaining.to_str()
            && let Ok(n) = s.parse::<u64>()
        {
            if n == 0 {
                return Err(AppError::GithubApi(
                    "rate limit exceeded — wait for reset".into(),
                ));
            }
            if n < LOW_RATE_LIMIT_THRESHOLD {
                eprintln!("warning: GitHub API rate limit low — {n} requests remaining");
            }
        }
        Ok(())
    }

    /// Map a non-2xx response to an AppError.
    fn handle_error_status(
        &self,
        status: u16,
        response: &mut ureq::http::Response<ureq::Body>,
    ) -> AppError {
        // Check for rate-limit exhaustion on 403
        if status == 403
            && let Some(remaining) = response.headers().get("x-ratelimit-remaining")
            && remaining.to_str().unwrap_or("") == "0"
        {
            return AppError::GithubApi("rate limit exceeded — wait for reset".into());
        }

        let body_text = response.body_mut().read_to_string().unwrap_or_default();

        match status {
            401 => AppError::GithubAuth(format!("authentication failed (HTTP 401): {body_text}")),
            404 => AppError::NotFound(format!("GitHub resource not found (HTTP 404): {body_text}")),
            429 => AppError::GithubApi(format!("rate limited (HTTP 429): {body_text}")),
            _ => AppError::GithubApi(format!("HTTP {status}: {body_text}")),
        }
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Get a single pull request's merge/close status by number (SH-49).
    ///
    /// `GET /repos/{owner}/{repo}/pulls/{number}` — the same shape as an
    /// issue lookup, since GitHub's REST API treats a pull request as an
    /// issue with a `pulls` endpoint of its own.
    pub fn get_pull_request(&self, number: u64) -> Result<PullRequestStatus, AppError> {
        let path = format!("/repos/{}/{}/pulls/{number}", self.owner, self.repo);
        let mut response = self
            .get(&path)
            .call()
            .map_err(|e| AppError::GithubApi(e.to_string()))?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(self.handle_error_status(status, &mut response));
        }
        self.check_rate_limit(&response)?;

        let pr: PullRequestStatus = response
            .body_mut()
            .read_json()
            .map_err(|e| AppError::GithubApi(format!("failed to parse pull request: {e}")))?;

        Ok(pr)
    }
}
