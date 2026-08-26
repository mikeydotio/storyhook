//! `story pr-check` — watching a story's linked pull requests for merge
//! (SH-49).
//!
//! The one PR-link operation that talks to GitHub, which is why it is the
//! one gated behind the `github-pr` feature, the same way the daemon's
//! background poll is gated (see `invoke::dispatch`). [`super::pr_link`]
//! documents why `link`/`unlink` are not gated the same way.
//!
//! # The second mandatory cross-repo check
//!
//! [`super::pr_link::PrLinkService::link`] refuses a `close_on_merge: true`
//! link whose repository matches none of the project's registered remotes
//! *at link time*. [`run_check`] re-reads those registrations **fresh, on
//! every call** — never from what was true when the link was made — and
//! silently skips (never acts on) any link whose `(owner, repo)` no longer
//! matches any of them. A project can register or unregister a GitHub remote
//! after a link exists; this is what keeps a stale link from being acted on
//! against the wrong repository once that happens.
//!
//! # One client per repository, not one for the whole run (SH-408)
//!
//! A project may have more than one registered GitHub remote — see
//! [`super::pr_link`]'s module doc for why that is ordinary rather than an
//! edge case, and why this file does not resolve down to a single
//! repository the way it once read a single `(owner, repo)` off the deleted
//! sync engine's config. [`run_check`] instead groups matching links by
//! their own `(owner, repo)` and builds one [`GithubApi`] per group, so a
//! multi-repository project is checked correctly and so that one
//! repository's API failure — an expired token, a rate limit, a repository
//! made private — cannot silently abort checking every other repository's
//! links in the same invocation. A failure is recorded per link and, if any
//! occurred, turns the whole call into an error (never a "successful"
//! message hiding a partial failure — the same doctrine SH-159 already
//! established for the sync engine this file survived).

use std::collections::BTreeMap;

use crate::domain::{StoryEvent, SuperState, has_children};
use crate::error::AppError;
use crate::github::api::{GithubApi, GithubApiFactory};
use crate::output::Response;
use crate::store::{ExpectedSeq, PrLink, ReadOps, Store, StoreError, StoryNo};

use super::github::RealGithubApiFactory;
use super::pr_link::{PrLinkService, configured_github_repos};
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
    /// one PR-link operation that spends one. [`AppError::Validation`] if the
    /// project has no registered GitHub remote at all. [`AppError::GithubApi`]
    /// if [`crate::github::api::GithubApi::get_pull_request`] failed for one
    /// or more links — see the module doc's "One client per repository"
    /// section for why that never aborts the rest of the run, only the exit
    /// code.
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
    let token = crate::github::require_github_token(ctx.github_token())?;
    let project = ctx.project();

    // Read fresh, every call — see the module doc's check. A link linked
    // against a remote registration that has since changed is not evidence
    // about today's.
    let configured = configured_github_repos(ctx)?;
    if configured.is_empty() {
        return Err(AppError::Validation(
            "this project has no GitHub repository, so there is nothing to check a linked \
             pull request against. `story pr-check` asks GitHub whether a linked pull request \
             merged, and the repositories it may ask about are the project's registered \
             origins. Register one with `story project link origin <url>`; `story project \
             show` lists what this project already holds."
                .to_string(),
        ));
    }

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

    // Mandatory security control #2: a link whose (owner, repo) matches none
    // of the project's registered GitHub remotes **right now** is skipped,
    // not acted on — a remote may have been registered or unregistered
    // since the link was made, and a client is only ever built for a
    // repository this project currently claims (below).
    let mut skipped: Vec<String> = Vec::new();
    let matching: Vec<(StoryNo, PrLink)> = candidates
        .into_iter()
        .filter(|(_, link)| {
            let matches = configured.iter().any(|repo| {
                repo.owner.eq_ignore_ascii_case(&link.owner)
                    && repo.repo.eq_ignore_ascii_case(&link.repo)
            });
            if !matches {
                skipped.push(link.url.clone());
            }
            matches
        })
        .collect();

    // One client per distinct repository among the matching links, built
    // lazily — see the module doc's "One client per repository" section.
    let mut clients: BTreeMap<(String, String), Box<dyn GithubApi>> = BTreeMap::new();

    let mut merged: Vec<String> = Vec::new();
    let mut closed_without_merging: Vec<String> = Vec::new();
    let mut closed_stories: Vec<String> = Vec::new();
    // Per-link GitHub API failures, isolated from one another: one
    // repository's error must not stop another repository's links in the
    // same run from being checked. Non-empty at the end turns this call
    // into an error — see the trailing check, and the module doc's SH-159
    // cross-reference.
    let mut errored: Vec<(String, String)> = Vec::new();

    for (story_no, link) in matching {
        let client = clients
            .entry((link.owner.clone(), link.repo.clone()))
            .or_insert_with(|| {
                factory.build(
                    token.expose().to_string(),
                    link.owner.clone(),
                    link.repo.clone(),
                )
            });
        let status = match client.get_pull_request(link.number) {
            Ok(status) => status,
            Err(err) => {
                errored.push((link.url.clone(), err.to_string()));
                continue;
            }
        };
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
                if link.close_on_merge && !row.archived && !has_children(&row.snapshot) {
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
                    ctx.provenance(),
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
                    ctx.provenance(),
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
        "checked {} of {total_candidates} linked pull request(s): {} merged, {} closed without \
         merging",
        total_candidates - errored.len(),
        merged.len(),
        closed_without_merging.len()
    );
    if !closed_stories.is_empty() {
        message.push_str(&format!("\nclosed: {}", closed_stories.join(", ")));
    }
    if !skipped.is_empty() {
        message.push_str(&format!(
            "\nskipped (repository matches none of this project's registered GitHub remotes): {}",
            skipped.join(", ")
        ));
    }
    if errored.is_empty() {
        return Ok(Response::Message(message));
    }
    // Per-link failures are never folded into a "successful" message at
    // exit 0 — the same doctrine SH-159 established for the sync engine
    // this file survived. Every repository that *did* answer is still
    // reflected above and, for a merge, already committed; only the
    // repositories that failed are missing from it.
    message.push_str("\nerrored (not checked — see below; other links were still checked):");
    for (url, detail) in &errored {
        message.push_str(&format!("\n  {url}: {detail}"));
    }
    Err(AppError::GithubApi(message))
}
