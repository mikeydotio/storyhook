//! Production factory for origin-bound gh pull-request reads.
use crate::github::api::{GithubApi, GithubApiFactory};
use crate::github::client::GithubClient;
use crate::github_access::Repository;

/// Builds the gh-backed client for a validated checkout origin.
pub struct RealGithubApiFactory;
impl GithubApiFactory for RealGithubApiFactory {
    fn build(&self, repository: Repository) -> Box<dyn GithubApi> {
        Box::new(GithubClient::new(repository))
    }
}
