//! `story link-pr` / `story unlink-pr` — linking GitHub pull requests to
//! stories (SH-49).
//!
//! # Why linking needs no GitHub access, and lives outside `github-sync`
//!
//! [`PrLinkService::link`] and [`PrLinkService::unlink`] never talk to
//! GitHub — a PR URL is parsed, not fetched — so they work in every build,
//! `github-sync` feature or not, and never spend a caller's token. That is a
//! deliberate design decision, not an accident of what happened to be easy:
//! see `.council/sh49-linked-prs/DECISION.md`, "Linking/unlinking is
//! feature-independent of github-sync — a pure event-sourced fact requiring
//! no network access." This module and [`crate::domain::pr_url`], which it
//! depends on, are consequently both ungated.
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
//! whose `(owner, repo)` disagrees with the project's *currently* configured
//! `github_sync` remote, at link time — see
//! [`configured_remote`](PrLinkService::configured_remote). `super::pr_check`
//! documents its own, second check of the same kind, taken fresh at check
//! time because the configured remote can change between the two.
//!
//! Neither check applies to a `close_on_merge: false` link — that is
//! deliberately a cross-repository bookmark, and nothing about it can close
//! anything.

use crate::domain::pr_url::parse_pr_url;
use crate::domain::{StoryEvent, StorySnapshot};
use crate::error::AppError;
use crate::store::{ExpectedSeq, ReadOps, Store};

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
    #[cfg(feature = "github-sync")]
    pub(super) fn ctx(&self) -> &'ctx Ctx<'ctx, S> {
        self.ctx
    }

    /// Links a pull request to an open story.
    ///
    /// Refuses a closed story, exactly like `commit-sync`'s
    /// `resolve_open_story` refusal. Refuses a `close_on_merge: true` link
    /// whose repository disagrees with the project's configured `github_sync`
    /// remote — see the module doc — unless the project has none configured,
    /// in which case there is nothing to validate against and the link is
    /// accepted as given.
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
            )?)
        })?)
    }

    /// Refuses `(owner, repo)` unless it matches this project's currently
    /// configured `github_sync` remote — or the project has none configured,
    /// in which case there is nothing to compare against and this is a no-op.
    ///
    /// Deliberately no override flag: the winning council proposal (Proposal
    /// B, `.council/sh49-linked-prs/DECISION.md`) is a hard block, and a
    /// caller that means a genuine cross-repository bookmark passes
    /// `close_on_merge: false` instead of asking this check to stand aside.
    fn refuse_cross_repo(&self, owner: &str, repo: &str) -> Result<(), AppError> {
        let Some((configured_owner, configured_repo)) = self.configured_remote()? else {
            return Ok(());
        };
        if configured_owner.eq_ignore_ascii_case(owner)
            && configured_repo.eq_ignore_ascii_case(repo)
        {
            return Ok(());
        }
        Err(AppError::Validation(format!(
            "pull request `{owner}/{repo}` does not match this project's configured GitHub \
             repository `{configured_owner}/{configured_repo}` — a `close_on_merge` link could \
             close this story on another repository's merge. Pass --no-close-on-merge (or \
             `close_on_merge: false` over the API) if you mean to bookmark a pull request in \
             another repository.",
        )))
    }

    /// This project's configured GitHub remote, `(owner, repo)`, if it has
    /// one.
    ///
    /// A deliberately narrow, ungated read: `github-sync`'s full
    /// `GithubSyncConfig` (`crate::github::sync_state`) is feature-gated, so
    /// [`refuse_cross_repo`](Self::refuse_cross_repo) — which must work
    /// without that feature — reads the two fields it needs straight off the
    /// raw JSON document `ReadOps::settings` returns rather than deserializing
    /// the whole thing.
    ///
    /// `None` for a missing document, a document with no `github` key, or one
    /// whose `owner`/`repo` aren't strings — every one of those means
    /// "nothing configured", exactly as [`refuse_cross_repo`](Self::refuse_cross_repo)
    /// already treats an absent document. A malformed document is not this
    /// method's job to report; `story doctor` is where that belongs.
    fn configured_remote(&self) -> Result<Option<(String, String)>, AppError> {
        let project = self.ctx.project();
        let document = self
            .ctx
            .store()
            .read(|tx| Ok(tx.settings(project)?.github_sync))?;
        let Some(document) = document else {
            return Ok(None);
        };
        let owner = document
            .get("github")
            .and_then(|github| github.get("owner"))
            .and_then(serde_json::Value::as_str);
        let repo = document
            .get("github")
            .and_then(|github| github.get("repo"))
            .and_then(serde_json::Value::as_str);
        Ok(match (owner, repo) {
            (Some(owner), Some(repo)) => Some((owner.to_string(), repo.to_string())),
            _ => None,
        })
    }
}
