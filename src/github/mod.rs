//! The GitHub REST API layer — a durable credential
//! ([`credential_store`]) and the one call [`api::GithubApi::get_pull_request`]
//! `story pr-check` and the daemon's background poll need.
//!
//! Until SH-408 this module was the story-to-GitHub-Issues sync engine: a
//! three-way merge, an issue-body metadata block, a first-run setup wizard,
//! and the config document they all shared. That engine is retired —
//! storyhook stories no longer sync to GitHub issues — and what survives is
//! everything `story pr-check`/`story link-pr` need, which never depended on
//! any of it: [`api`] and [`client`] talk to GitHub, [`types::PullRequestStatus`]
//! is the one shape they hand back, and [`credential_store`] is the OS
//! keychain `story github-auth` and the poll thread share.
//!
//! `story link-pr`/`unlink-pr` themselves, and the URL-parsing grammar they
//! and `story pr-check` both need to recognize a registered remote as a
//! GitHub repository, live in [`crate::domain::github_remote`] and
//! [`crate::domain::pr_url`] instead — ungated, because linking must work in
//! every build (SH-49's council verdict) and both callers of the remote
//! grammar are on that side of the feature boundary too (SH-408).

pub mod api;
pub mod client;
pub mod credential_store;
pub mod types;

use crate::domain::secret::{self, GithubToken};
use crate::error::AppError;

/// The caller's GitHub credential, or the refusal that names what to do.
///
/// **This used to be a `std::env::var` read, and that was the defect** (SH-153).
/// Since SH-114 this code runs inside the daemon, whose environment is a
/// snapshot of whichever client happened to start it — so a caller who exported
/// a token was told it was unset, and a caller who had not exported one silently
/// spent the token of whoever had. The credential is read by the client now and
/// travels in the request envelope; this function is only the refusal.
///
/// # Errors
///
/// [`AppError::GithubAuth`] when the caller supplied no credential.
pub fn require_github_token(token: Option<&GithubToken>) -> Result<&GithubToken, AppError> {
    token.ok_or_else(|| AppError::GithubAuth(secret::NO_TOKEN.to_string()))
}
