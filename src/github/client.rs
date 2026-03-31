use std::time::Duration;

use ureq::Agent;

use crate::error::AppError;

use super::types::{
    CreateIssueRequest, GithubComment, GithubIssue, TimelineEvent, UpdateIssueRequest,
};

const API_BASE: &str = "https://api.github.com";
const PER_PAGE: u32 = 100;
const LOW_RATE_LIMIT_THRESHOLD: u64 = 50;

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

    /// Build a POST request with common headers.
    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let url = format!("{API_BASE}{path}");
        self.agent
            .post(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "storyhook")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// Build a PATCH request with common headers.
    fn patch(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let url = format!("{API_BASE}{path}");
        self.agent
            .patch(&url)
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
        if let Some(remaining) = response.headers().get("x-ratelimit-remaining") {
            if let Ok(s) = remaining.to_str() {
                if let Ok(n) = s.parse::<u64>() {
                    if n == 0 {
                        return Err(AppError::GithubApi(
                            "rate limit exceeded — wait for reset".into(),
                        ));
                    }
                    if n < LOW_RATE_LIMIT_THRESHOLD {
                        eprintln!(
                            "warning: GitHub API rate limit low — {n} requests remaining"
                        );
                    }
                }
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
        if status == 403 {
            if let Some(remaining) = response.headers().get("x-ratelimit-remaining") {
                if remaining.to_str().unwrap_or("") == "0" {
                    return AppError::GithubApi(
                        "rate limit exceeded — wait for reset".into(),
                    );
                }
            }
        }

        let body_text = response
            .body_mut()
            .read_to_string()
            .unwrap_or_default();

        match status {
            401 => AppError::GithubAuth(format!(
                "authentication failed (HTTP 401): {body_text}"
            )),
            404 => AppError::NotFound(format!(
                "GitHub resource not found (HTTP 404): {body_text}"
            )),
            429 => AppError::GithubApi(format!(
                "rate limited (HTTP 429): {body_text}"
            )),
            _ => AppError::GithubApi(format!("HTTP {status}: {body_text}")),
        }
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Validate the token by fetching repository metadata.
    pub fn validate_token(&self) -> Result<(), AppError> {
        let path = format!("/repos/{}/{}", self.owner, self.repo);
        let mut response = self.get(&path).call().map_err(|e| {
            AppError::GithubApi(e.to_string())
        })?;

        let status = response.status().as_u16();
        if status == 401 {
            return Err(AppError::GithubAuth(
                "invalid or expired GitHub token".into(),
            ));
        }
        if !response.status().is_success() {
            return Err(self.handle_error_status(status, &mut response));
        }
        self.check_rate_limit(&response)?;
        // Drain the body so the connection can be reused
        let _ = response.body_mut().read_to_string();
        Ok(())
    }

    /// List issues for the repository, with optional `since` filter and state.
    ///
    /// Automatically paginates and filters out pull requests.
    pub fn list_issues(
        &self,
        since: Option<&str>,
        state: &str,
    ) -> Result<Vec<GithubIssue>, AppError> {
        let mut all_issues = Vec::new();
        let mut page: u32 = 1;

        loop {
            let path = format!("/repos/{}/{}/issues", self.owner, self.repo);
            let mut req = self
                .get(&path)
                .query("state", state)
                .query("per_page", &PER_PAGE.to_string())
                .query("page", &page.to_string())
                .query("sort", "updated")
                .query("direction", "asc");

            if let Some(since_val) = since {
                req = req.query("since", since_val);
            }

            let mut response = req.call().map_err(|e| {
                AppError::GithubApi(e.to_string())
            })?;

            let status = response.status().as_u16();
            if !response.status().is_success() {
                return Err(self.handle_error_status(status, &mut response));
            }
            self.check_rate_limit(&response)?;

            let issues: Vec<GithubIssue> = response
                .body_mut()
                .read_json()
                .map_err(|e| AppError::GithubApi(format!("failed to parse issues: {e}")))?;

            let count = issues.len();

            // Filter out pull requests — GitHub's issue endpoint includes them
            all_issues.extend(issues.into_iter().filter(|i| !i.is_pull_request()));

            if count < PER_PAGE as usize {
                break;
            }
            page += 1;
        }

        Ok(all_issues)
    }

    /// Get a single issue by number.
    pub fn get_issue(&self, number: u64) -> Result<GithubIssue, AppError> {
        let path = format!("/repos/{}/{}/issues/{number}", self.owner, self.repo);
        let mut response = self.get(&path).call().map_err(|e| {
            AppError::GithubApi(e.to_string())
        })?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(self.handle_error_status(status, &mut response));
        }
        self.check_rate_limit(&response)?;

        let issue: GithubIssue = response
            .body_mut()
            .read_json()
            .map_err(|e| AppError::GithubApi(format!("failed to parse issue: {e}")))?;

        Ok(issue)
    }

    /// Create a new issue.
    pub fn create_issue(
        &self,
        req: &CreateIssueRequest,
    ) -> Result<GithubIssue, AppError> {
        let path = format!("/repos/{}/{}/issues", self.owner, self.repo);
        let mut response = self
            .post(&path)
            .send_json(req)
            .map_err(|e| AppError::GithubApi(e.to_string()))?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(self.handle_error_status(status, &mut response));
        }
        self.check_rate_limit(&response)?;

        let issue: GithubIssue = response
            .body_mut()
            .read_json()
            .map_err(|e| AppError::GithubApi(format!("failed to parse created issue: {e}")))?;

        Ok(issue)
    }

    /// Update an existing issue.
    pub fn update_issue(
        &self,
        number: u64,
        req: &UpdateIssueRequest,
    ) -> Result<GithubIssue, AppError> {
        let path = format!("/repos/{}/{}/issues/{number}", self.owner, self.repo);
        let mut response = self
            .patch(&path)
            .send_json(req)
            .map_err(|e| AppError::GithubApi(e.to_string()))?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(self.handle_error_status(status, &mut response));
        }
        self.check_rate_limit(&response)?;

        let issue: GithubIssue = response
            .body_mut()
            .read_json()
            .map_err(|e| AppError::GithubApi(format!("failed to parse updated issue: {e}")))?;

        Ok(issue)
    }

    /// List comments on an issue, with optional `since` filter.
    pub fn list_comments(
        &self,
        issue_number: u64,
        since: Option<&str>,
    ) -> Result<Vec<GithubComment>, AppError> {
        let mut all_comments = Vec::new();
        let mut page: u32 = 1;

        loop {
            let path = format!(
                "/repos/{}/{}/issues/{issue_number}/comments",
                self.owner, self.repo
            );
            let mut req = self
                .get(&path)
                .query("per_page", &PER_PAGE.to_string())
                .query("page", &page.to_string());

            if let Some(since_val) = since {
                req = req.query("since", since_val);
            }

            let mut response = req.call().map_err(|e| {
                AppError::GithubApi(e.to_string())
            })?;

            let status = response.status().as_u16();
            if !response.status().is_success() {
                return Err(self.handle_error_status(status, &mut response));
            }
            self.check_rate_limit(&response)?;

            let comments: Vec<GithubComment> = response
                .body_mut()
                .read_json()
                .map_err(|e| {
                    AppError::GithubApi(format!("failed to parse comments: {e}"))
                })?;

            let count = comments.len();
            all_comments.extend(comments);

            if count < PER_PAGE as usize {
                break;
            }
            page += 1;
        }

        Ok(all_comments)
    }

    /// Create a comment on an issue.
    pub fn create_comment(
        &self,
        issue_number: u64,
        body: &str,
    ) -> Result<GithubComment, AppError> {
        let path = format!(
            "/repos/{}/{}/issues/{issue_number}/comments",
            self.owner, self.repo
        );

        #[derive(serde::Serialize)]
        struct CommentBody<'a> {
            body: &'a str,
        }

        let mut response = self
            .post(&path)
            .send_json(&CommentBody { body })
            .map_err(|e| AppError::GithubApi(e.to_string()))?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(self.handle_error_status(status, &mut response));
        }
        self.check_rate_limit(&response)?;

        let comment: GithubComment = response
            .body_mut()
            .read_json()
            .map_err(|e| {
                AppError::GithubApi(format!("failed to parse created comment: {e}"))
            })?;

        Ok(comment)
    }

    /// Get timeline events for an issue, with optional `since` filter.
    ///
    /// Automatically paginates. When `since` is provided, only events with
    /// `created_at` after that timestamp are returned.
    pub fn get_timeline(
        &self,
        issue_number: u64,
        since: Option<&str>,
    ) -> Result<Vec<TimelineEvent>, AppError> {
        let mut all_events = Vec::new();
        let mut page: u32 = 1;

        loop {
            let path = format!(
                "/repos/{}/{}/issues/{issue_number}/timeline",
                self.owner, self.repo
            );
            let req = self
                .get(&path)
                .header("Accept", "application/vnd.github.mockingbird-preview+json")
                .query("per_page", &PER_PAGE.to_string())
                .query("page", &page.to_string());

            let mut response = req.call().map_err(|e| {
                AppError::GithubApi(e.to_string())
            })?;

            let status = response.status().as_u16();
            if !response.status().is_success() {
                return Err(self.handle_error_status(status, &mut response));
            }
            self.check_rate_limit(&response)?;

            let events: Vec<TimelineEvent> = response
                .body_mut()
                .read_json()
                .map_err(|e| {
                    AppError::GithubApi(format!("failed to parse timeline: {e}"))
                })?;

            let count = events.len();

            if let Some(since_val) = since {
                all_events.extend(events.into_iter().filter(|ev| {
                    ev.created_at
                        .as_deref()
                        .is_some_and(|ts| ts > since_val)
                }));
            } else {
                all_events.extend(events);
            }

            if count < PER_PAGE as usize {
                break;
            }
            page += 1;
        }

        Ok(all_events)
    }
}
