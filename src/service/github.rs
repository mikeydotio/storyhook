//! The production [`GithubApiFactory`] — the store-independent half of the
//! GitHub integration.
//!
//! Until SH-408 this module was the story↔GitHub-Issues sync engine's own
//! service layer: [`GithubSyncService`], the [`SyncStorage`] backed by the
//! store, and the remote-reconciliation helper `story project link origin`
//! ran on restore. That engine is retired; [`RealGithubApiFactory`] is the
//! one piece [`super::pr_check`] still needs, and it never depended on any
//! of the deleted machinery — it is a bare adapter from
//! [`GithubApiFactory::build`] onto [`GithubClient::new`].

use crate::github::api::{GithubApi, GithubApiFactory};
use crate::github::client::GithubClient;

/// [`GithubApiFactory`] backed by the real GitHub REST client.
pub struct RealGithubApiFactory;

impl GithubApiFactory for RealGithubApiFactory {
    fn build(&self, token: String, owner: String, repo: String) -> Box<dyn GithubApi> {
        Box::new(GithubClient::new(token, owner, repo))
    }
}
