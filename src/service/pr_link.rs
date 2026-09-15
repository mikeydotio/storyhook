//! PR links are local facts; automatic close-on-merge authority comes only
//! from the registered project's current checkout origin (SH-734).
//! Informational links may name other repositories. Linking requires no gh
//! installation or network, and remains available without github-pr.

use crate::domain::github_remote::{GithubRepo, parse_github_url};
use crate::domain::pr_url::parse_pr_url;
use crate::domain::{StoryEvent, StorySnapshot};
use crate::error::AppError;
use crate::store::{ExpectedSeq, ProjectRemoteRecord, ReadOps, Store};

use super::{Ctx, append_and_fold, project_prefix, resolve_open_story};

/// Linked-PR management over one project in one store.
pub struct PrLinkService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> PrLinkService<'ctx, S> {
    /// A PR-link service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// The context this service is bound to.
    ///
    /// `pub(super)` rather than private: `super::pr_check`'s `impl` block for
    /// this same type — in its own, feature-gated file — needs it, and a
    /// struct field is private to this module even to an `impl` of the same
    /// type declared elsewhere. Gated the same way `pr_check` is: nothing
    /// outside that feature calls it, and an unused-but-public accessor is
    /// exactly the dead code `#[warn(dead_code)]` exists to catch.
    #[cfg(feature = "github-pr")]
    pub(super) fn ctx(&self) -> &'ctx Ctx<'ctx, S> {
        self.ctx
    }

    /// Links a pull request to an open story.
    ///
    /// Refuses a closed story — `Intent::Edit`, not `Intent::Append`. A PR
    /// link is not an observation of what already happened the way a commit
    /// link or a comment is: `close_on_merge: true` is a standing instruction
    /// to move the story in the *future*, on a webhook this call cannot see
    /// yet, so it is refused for the same reason any other edit is (SH-279 —
    /// `commit-sync`'s own link stopped refusing a closed story once its
    /// observation-only argument was made explicit; nothing about that
    /// argument reaches this write). Refuses a `close_on_merge: true` link
    /// whose repository differs from the registered checkout's current origin.
    /// An unavailable checkout cannot authorize an automatic state change.
    ///
    /// Re-linking a PR this story already links **upserts**: the new
    /// `close_on_merge` value replaces the old one, which is how a caller
    /// flips the flag on an existing link. See
    /// `store::sqlite::write::project_pr_link`.
    pub fn link(
        &self,
        id: &str,
        url: &str,
        close_on_merge: bool,
    ) -> Result<StorySnapshot, AppError> {
        let reference = parse_pr_url(url)?;
        if close_on_merge {
            self.refuse_cross_repo(&reference.host, &reference.owner, &reference.repo)?;
        }

        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            Ok(append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryPrLinked {
                    at: now.clone(),
                    url: url.to_string(),
                    owner: reference.owner,
                    repo: reference.repo,
                    number: reference.number,
                    close_on_merge,
                }],
                self.ctx.provenance(),
            )?)
        })?)
    }

    /// Unlinks a previously-linked pull request from an open story, by URL.
    pub fn unlink(&self, id: &str, url: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            Ok(append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryPrUnlinked {
                    at: now.clone(),
                    url: url.to_string(),
                }],
                self.ctx.provenance(),
            )?)
        })?)
    }

    /// Refuses `(host, owner, repo)` unless it matches at least one of this
    /// project's registered GitHub remotes — or the project has none
    /// registered, in which case there is nothing to compare against and
    /// this is a no-op.
    ///
    /// Deliberately no override flag: the winning council proposal (Proposal
    /// B on SH-49) is a hard block, and a
    /// caller that means a genuine cross-repository bookmark passes
    /// `close_on_merge: false` instead of asking this check to stand aside.
    fn refuse_cross_repo(&self, host: &str, owner: &str, repo: &str) -> Result<(), AppError> {
        let configured = configured_github_repos(self.ctx)?;
        if configured.iter().any(|c| {
            c.host.eq_ignore_ascii_case(host)
                && c.owner.eq_ignore_ascii_case(owner)
                && c.repo.eq_ignore_ascii_case(repo)
        }) {
            return Ok(());
        }
        Err(AppError::Validation(format!(
            "pull request `{host}/{owner}/{repo}` does not match this project's current checkout origin ({}) — a `close_on_merge` link could close this story on another \
             repository's merge. Pass --no-close-on-merge (or `close_on_merge: false` over the \
             API) if you mean to bookmark a pull request in another repository.",
            configured
                .iter()
                .map(|c| format!("{}/{}/{}", c.host, c.owner, c.repo))
                .collect::<Vec<_>>()
                .join(", "),
        )))
    }
}

/// The project's current origin, validated against its registered checkout identity.
pub(crate) fn configured_github_repos<S: Store>(
    ctx: &Ctx<'_, S>,
) -> Result<Vec<GithubRepo>, AppError> {
    Ok(vec![
        super::github_repository::repository(ctx)?
            .identity()
            .clone(),
    ])
}

/// Parses, sorts, and deduplicates the GitHub repositories in stored remotes.
pub(crate) fn github_repos_from_remotes(remotes: &[ProjectRemoteRecord]) -> Vec<GithubRepo> {
    let mut repos: Vec<GithubRepo> = remotes
        .iter()
        .filter_map(|remote| parse_github_url(&remote.raw))
        .collect();
    repos.sort();
    repos.dedup();
    repos
}
