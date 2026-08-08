//! `story pr-check` — watching a story's linked pull requests for merge
//! (SH-49).
//!
//! The one PR-link operation that talks to GitHub, which is why it is the
//! one gated behind the `github-sync` feature, the same way `story
//! github-sync` is gated (see `invoke::dispatch`). [`super::pr_link`]
//! documents why `link`/`unlink` are not gated the same way.
//!
//! # The second mandatory cross-repo check
//!
//! [`super::pr_link::PrLinkService::link`] refuses a `close_on_merge: true`
//! link whose repository disagrees with the project's remote *at link time*.
//! [`run_check`] re-reads that configuration **fresh, on every call** — never
//! from what was true when the link was made — and silently skips (never
//! acts on) any link whose `(owner, repo)` no longer matches. A project can
//! repoint its GitHub remote after a link exists; this is what keeps a stale
//! link from being acted on against the wrong repository once that happens.

use crate::domain::{StoryEvent, SuperState};
use crate::error::AppError;
use crate::github::api::GithubApiFactory;
use crate::github::storage::SyncStorage;
use crate::output::Response;
use crate::store::{ExpectedSeq, PrLink, ReadOps, Store, StoreError, StoryNo};

use super::github::{RealGithubApiFactory, StoreSyncStorage};
use super::pr_link::PrLinkService;
use super::story::state_transition_events;
use super::{Ctx, append_and_fold, project_prefix, resolve_story};

impl<'ctx, S: Store> PrLinkService<'ctx, S> {
    /// Checks one story's (or, with `id: None`, every open story's) linked
    /// pull requests against GitHub, closing a story whose merged link asked
    /// to be closed on merge.
    ///
    /// # Errors
    ///
    /// [`AppError::GithubAuth`] if no GitHub token was supplied — this is the
    /// one PR-link operation that spends one. Whatever
    /// [`crate::github::api::GithubApi::get_pull_request`] returns for a
    /// given link's own repository otherwise propagates.
    pub fn check(&self, id: Option<&str>) -> Result<Response, AppError> {
        run_check(self.ctx(), &RealGithubApiFactory, id)
    }
}

/// [`PrLinkService::check`]'s engine, taking the [`GithubApiFactory`] as a
/// parameter — the same seam `crate::github::run_sync_with` uses, so a test
/// can substitute `storyhook_test_support::FakeGithubApiFactory` without a
/// network. `PrLinkService::check` is the production wrapper, fixed to
/// [`RealGithubApiFactory`].
pub fn run_check<S: Store>(
    ctx: &Ctx<'_, S>,
    factory: &dyn GithubApiFactory,
    id: Option<&str>,
) -> Result<Response, AppError> {
    let token = crate::github::initial::require_github_token(ctx.github_token())?;
    let project = ctx.project();

    // Read fresh, every call — see the module doc's check. A link linked
    // against yesterday's remote is not evidence about today's.
    let Some(config) = StoreSyncStorage::new(ctx).load_config()? else {
        return Err(AppError::Validation(
            "no GitHub remote is configured for this project — run `story github-sync` \
             (or configure one) before `story pr-check`"
                .to_string(),
        ));
    };

    let prefix = ctx.store().read(|tx| project_prefix(tx, project))?;
    let candidates: Vec<(StoryNo, PrLink)> = match id {
        Some(id) => {
            let story_no = ctx
                .store()
                .read(|tx| Ok(resolve_story(tx, project, &prefix, id)?.0))?;
            ctx.store()
                .read(|tx| tx.open_pr_links_for_story(project, story_no))?
                .into_iter()
                .map(|link| (story_no, link))
                .collect()
        }
        None => ctx.store().read(|tx| tx.open_pr_links(project))?,
    };
    let total_candidates = candidates.len();

    // Mandatory security control #2: a link whose (owner, repo) no longer
    // matches what the project is configured to sync with **right now** is
    // skipped, not acted on — the remote may have been repointed since the
    // link was made, and a client scoped to the configured remote (below)
    // has no business asking about a pull request on a different one.
    let mut skipped: Vec<String> = Vec::new();
    let matching: Vec<(StoryNo, PrLink)> = candidates
        .into_iter()
        .filter(|(_, link)| {
            let matches = link.owner.eq_ignore_ascii_case(&config.github.owner)
                && link.repo.eq_ignore_ascii_case(&config.github.repo);
            if !matches {
                skipped.push(link.url.clone());
            }
            matches
        })
        .collect();

    let client = factory.build(
        token.expose().to_string(),
        config.github.owner.clone(),
        config.github.repo.clone(),
    );

    let mut merged: Vec<String> = Vec::new();
    let mut closed_without_merging: Vec<String> = Vec::new();
    let mut closed_stories: Vec<String> = Vec::new();

    for (story_no, link) in matching {
        let status = client.get_pull_request(link.number)?;
        let now = ctx.now();

        if status.merged {
            merged.push(link.url.clone());
            ctx.store().write(|tx| {
                let row = tx
                    .story(project, story_no)?
                    .ok_or_else(|| StoreError::NotFound(format!("story {story_no} not found")))?;
                let states = tx.state_map(project)?;
                let mut events = vec![StoryEvent::StoryPrMerged {
                    at: now.clone(),
                    url: link.url.clone(),
                }];
                // Only when the link asked for it, and only while there is
                // still something to close — a story a person already closed
                // by hand is not reopened-and-reclosed by this.
                if link.close_on_merge && !row.archived {
                    let closed_state = states
                        .values()
                        .find(|state| state.super_state == SuperState::Closed)
                        .cloned()
                        .ok_or_else(|| {
                            StoreError::Invariant("project has no CLOSED-mapped state".to_string())
                        })?;
                    events.extend(state_transition_events(
                        &closed_state,
                        row.awaiting.is_some(),
                        &now,
                        Vec::new(),
                    ));
                    closed_stories.push(story_no.to_id(&prefix));
                }
                append_and_fold(
                    tx,
                    project,
                    story_no,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(row.head_seq),
                    &events,
                )?;
                Ok(())
            })?;
        } else if status.state == "closed" {
            closed_without_merging.push(link.url.clone());
            ctx.store().write(|tx| {
                let row = tx
                    .story(project, story_no)?
                    .ok_or_else(|| StoreError::NotFound(format!("story {story_no} not found")))?;
                let states = tx.state_map(project)?;
                append_and_fold(
                    tx,
                    project,
                    story_no,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(row.head_seq),
                    &[StoryEvent::StoryPrClosed {
                        at: now.clone(),
                        url: link.url.clone(),
                    }],
                )?;
                Ok(())
            })?;
        }
        // Still open: nothing changed, so nothing is written. See the design
        // doc's note on `last_checked_at` — persisting "nothing happened" has
        // no precedent in this store and no consumer yet, so it is skipped
        // rather than inventing a fifth event kind to record it.
    }

    let mut message = format!(
        "checked {total_candidates} linked pull request(s): {} merged, {} closed without merging",
        merged.len(),
        closed_without_merging.len()
    );
    if !closed_stories.is_empty() {
        message.push_str(&format!("\nclosed: {}", closed_stories.join(", ")));
    }
    if !skipped.is_empty() {
        message.push_str(&format!(
            "\nskipped (repository no longer matches this project's configured remote): {}",
            skipped.join(", ")
        ));
    }
    Ok(Response::Message(message))
}
