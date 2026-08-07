//! The seam between the sync engine and GitHub itself.
//!
//! [`GithubApi`] names exactly the 7 calls the pull phase, the push phase and
//! initial setup make against a GitHub repository — mirroring
//! [`super::storage::SyncStorage`]'s own rationale: a trait names what the
//! engine needs, not everything the concrete client can do. [`GithubClient`]
//! stays the only production implementation, unchanged; a `get_timeline`
//! caller would find it on `GithubClient` directly, not through this trait,
//! because nothing under `src/github/` calls it (tracked as its own
//! dead-code finding, SH-198).
//!
//! `dyn`, not a generic parameter, for the same reason `SyncStorage` is: this
//! is a network-bound command whose cost is measured in HTTP round trips, so
//! monomorphizing the engine per implementation buys nothing.
//!
//! [`GithubApiFactory`] exists because owner/repo are not known at either of
//! [`super::run_sync_with`]'s or [`super::initial::run_initial_setup`]'s
//! point of call — one reads them from stored config, the other from a
//! freshly detected git remote — so neither function can be handed a
//! ready-built [`GithubApi`]. Both take a factory instead and build the
//! client once owner/repo are in hand, exactly where each already calls
//! `GithubClient::new`. [`crate::service::RealGithubApiFactory`] is the
//! production implementation; `storyhook_test_support::FakeGithubApiFactory`
//! is the test one.

use super::client::GithubClient;
use super::types::{CreateIssueRequest, GithubComment, GithubIssue, UpdateIssueRequest};
use crate::error::AppError;

/// The 7 GitHub REST calls the sync engine makes.
pub trait GithubApi {
    /// Validates the credential by fetching repository metadata.
    fn validate_token(&self) -> Result<(), AppError>;

    /// Lists issues, with optional `since` filter and state.
    fn list_issues(&self, since: Option<&str>, state: &str) -> Result<Vec<GithubIssue>, AppError>;

    /// Gets a single issue by number.
    fn get_issue(&self, number: u64) -> Result<GithubIssue, AppError>;

    /// Creates a new issue.
    fn create_issue(&self, req: &CreateIssueRequest) -> Result<GithubIssue, AppError>;

    /// Updates an existing issue.
    fn update_issue(&self, number: u64, req: &UpdateIssueRequest) -> Result<GithubIssue, AppError>;

    /// Lists comments on an issue, with optional `since` filter.
    fn list_comments(
        &self,
        issue_number: u64,
        since: Option<&str>,
    ) -> Result<Vec<GithubComment>, AppError>;

    /// Creates a comment on an issue.
    fn create_comment(&self, issue_number: u64, body: &str) -> Result<GithubComment, AppError>;
}

impl GithubApi for GithubClient {
    fn validate_token(&self) -> Result<(), AppError> {
        self.validate_token()
    }

    fn list_issues(&self, since: Option<&str>, state: &str) -> Result<Vec<GithubIssue>, AppError> {
        self.list_issues(since, state)
    }

    fn get_issue(&self, number: u64) -> Result<GithubIssue, AppError> {
        self.get_issue(number)
    }

    fn create_issue(&self, req: &CreateIssueRequest) -> Result<GithubIssue, AppError> {
        self.create_issue(req)
    }

    fn update_issue(&self, number: u64, req: &UpdateIssueRequest) -> Result<GithubIssue, AppError> {
        self.update_issue(number, req)
    }

    fn list_comments(
        &self,
        issue_number: u64,
        since: Option<&str>,
    ) -> Result<Vec<GithubComment>, AppError> {
        self.list_comments(issue_number, since)
    }

    fn create_comment(&self, issue_number: u64, body: &str) -> Result<GithubComment, AppError> {
        self.create_comment(issue_number, body)
    }
}

/// Builds a [`GithubApi`] once the repository it talks to is known.
///
/// See the module doc for why this indirection exists rather than passing a
/// ready-built client.
pub trait GithubApiFactory {
    /// A client for `owner/repo`, authenticated with `token`.
    fn build(&self, token: String, owner: String, repo: String) -> Box<dyn GithubApi>;
}
