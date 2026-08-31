//! `story link-pr` / `story unlink-pr` — linking GitHub pull requests to
//! stories (SH-49).
//!
//! # Why linking needs no GitHub access, and lives outside `github-pr`
//!
//! [`PrLinkService::link`] and [`PrLinkService::unlink`] never talk to
//! GitHub — a PR URL is parsed, not fetched — so they work in every build,
//! `github-pr` feature or not, and never spend a caller's token. That is a
//! deliberate design decision, not an accident of what happened to be easy:
//! see SH-49's council verdict, "Linking/unlinking is
//! feature-independent of github-sync — a pure event-sourced fact requiring
//! no network access" (the feature it names is `github-pr` since SH-408
//! renamed it). This module and [`crate::domain::pr_url`], which it depends
//! on, are consequently both ungated.
//!
//! `story pr-check` — the one operation that does talk to GitHub, to learn
//! whether a linked pull request merged — lives in the sibling,
//! feature-gated `super::pr_check` instead, as a second `impl` block on
//! [`PrLinkService`] itself.
//!
//! # The mandatory cross-repo check
//!
//! A story's `close_on_merge` link is a promise that merging *someone else's*
//! pull request will close *this* story. That promise is only safe to keep
//! automatically when the pull request belongs to the repository this project
//! is actually configured to sync with — otherwise a story closes itself
//! because an unrelated repository merged an unrelated PR that happened to be
//! linked in by URL.
//!
//! [`PrLinkService::link`] refuses to record a `close_on_merge: true` link
//! whose `(owner, repo)` matches none of the project's *registered* GitHub
//! remotes, at link time — see [`configured_github_repos`]. `super::pr_check`
//! documents its own, second check of the same kind, taken fresh at check
//! time because the registered remotes can change between the two.
//!
//! # Why this is a membership test, not a resolve-to-one lookup (SH-408)
//!
//! A project may register more than one GitHub remote — a repository that
//! moved, a second canonical remote (`src/store/schema/0006_project_remotes.
//! sql`'s own header calls this an ordinary configuration, not a
//! hypothetical one). Neither caller here ever needs to pick "the" one
//! repository: [`parse_pr_url`] always yields a full, already-known
//! `(owner, repo, number)` before this guard runs, and a [`crate::store::
//! PrLink`] row stores its own `owner`/`repo` from link time. The question
//! both callers ask is "is this PR's repository one I registered?", which
//! has exactly one correct answer regardless of how many remotes are
//! registered — never an ambiguous one to refuse. A council convened on this
//! exact question (SH-408, 3-0) rejected resolving to a single value and
//! refusing on 2+ registered remotes: the daemon's background poll discards
//! [`super::pr_check::run_check`]'s result with no channel back to a human,
//! so a refusal there would silently and permanently disable close-on-merge
//! for any project with two legitimate remotes.
//!
//! Neither check applies to a `close_on_merge: false` link — that is
//! deliberately a cross-repository bookmark, and nothing about it can close
//! anything.

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
    /// whose repository matches none of the project's registered GitHub
    /// remotes — see the module doc — unless the project has none
    /// registered, in which case there is nothing to validate against and
    /// the link is accepted as given.
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
        let (owner, repo, number) = parse_pr_url(url)?;
        if close_on_merge {
            self.refuse_cross_repo(&owner, &repo)?;
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
                    owner,
                    repo,
                    number,
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

    /// Refuses `(owner, repo)` unless it matches at least one of this
    /// project's registered GitHub remotes — or the project has none
    /// registered, in which case there is nothing to compare against and
    /// this is a no-op.
    ///
    /// Deliberately no override flag: the winning council proposal (Proposal
    /// B on SH-49) is a hard block, and a
    /// caller that means a genuine cross-repository bookmark passes
    /// `close_on_merge: false` instead of asking this check to stand aside.
    fn refuse_cross_repo(&self, owner: &str, repo: &str) -> Result<(), AppError> {
        let configured = configured_github_repos(self.ctx)?;
        if configured.is_empty()
            || configured
                .iter()
                .any(|c| c.owner.eq_ignore_ascii_case(owner) && c.repo.eq_ignore_ascii_case(repo))
        {
            return Ok(());
        }
        Err(AppError::Validation(format!(
            "pull request `{owner}/{repo}` does not match any GitHub repository this project \
             has registered ({}) — a `close_on_merge` link could close this story on another \
             repository's merge. Pass --no-close-on-merge (or `close_on_merge: false` over the \
             API) if you mean to bookmark a pull request in another repository.",
            configured
                .iter()
                .map(|c| format!("{}/{}", c.owner, c.repo))
                .collect::<Vec<_>>()
                .join(", "),
        )))
    }
}

/// Every GitHub repository this project has a registered git origin for.
///
/// Empty when the project has no origin on `github.com` registered at all —
/// the same "nothing configured" state
/// [`refuse_cross_repo`](PrLinkService::refuse_cross_repo) already treats as
/// *accept*, and the state [`super::pr_check::run_check`] refuses under
/// (nothing to scope a GitHub API client to).
///
/// A **set**, never resolved down to one value, and never refused for
/// holding more than one member — see the module doc's "Why this is a
/// membership test" section for the council verdict (SH-408) this codifies.
///
/// Reads [`ReadOps::project_remotes`] — the store's registered origins —
/// never `git remote get-url` in a checkout: `crate::daemon::github_poll`'s
/// background poll has no checkout to read one from, and SH-112 made git an
/// optional convenience layer for a fact the store already holds
/// authoritatively. A project that has never run `story project link
/// origin` is consequently invisible here even with a perfectly good
/// `origin` in its working tree — registering it is what makes a project
/// reachable at all (SH-116), and this reads the same registration.
pub(crate) fn configured_github_repos<S: Store>(
    ctx: &Ctx<'_, S>,
) -> Result<Vec<GithubRepo>, AppError> {
    let project = ctx.project();
    let remotes = ctx.store().read(|tx| tx.project_remotes(project))?;
    Ok(github_repos_from_remotes(&remotes))
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
