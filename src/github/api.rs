//! The seam between `story pr-check` and GitHub itself.
//!
//! [`GithubApi`] once named the 7 calls the story-to-issue sync engine's
//! pull phase, push phase and initial setup made against a GitHub
//! repository, plus [`get_pull_request`](GithubApi::get_pull_request), the
//! one `story pr-check` makes. SH-408 retired that engine; this trait keeps
//! only the call that survived it. [`GithubClient`] stays the only
//! production implementation.
//!
//! `dyn`, not a generic parameter: this is a network-bound command whose
//! cost is measured in HTTP round trips, so monomorphizing the caller per
//! implementation buys nothing.
//!
//! [`GithubApiFactory`] exists because owner/repo are not known at
//! [`crate::service::pr_check::run_check`]'s point of call — it groups a
//! run's matching links by repository and builds one client per group.
//! [`crate::service::github::RealGithubApiFactory`] is the production
//! implementation; `storyhook_test_support::FakeGithubApiFactory` is the
//! test one.

use super::client::GithubClient;
use super::types::PullRequestStatus;
use crate::error::AppError;

/// The one GitHub REST call `story pr-check` makes (SH-49).
pub trait GithubApi {
    /// Gets a single pull request's merge/close status by number.
    fn get_pull_request(&self, number: u64) -> Result<PullRequestStatus, AppError>;
}

impl GithubApi for GithubClient {
    fn get_pull_request(&self, number: u64) -> Result<PullRequestStatus, AppError> {
        self.get_pull_request(number)
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
