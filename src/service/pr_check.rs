//! `story pr-check` — watching a story's linked pull requests for merge
//! (SH-49).
//!
//! The one PR-link operation that talks to GitHub, which is why it is the
//! one gated behind the `github-pr` feature, the same way the daemon's
//! background poll is gated (see `invoke::dispatch`). [`super::pr_link`]
//! documents why `link`/`unlink` are not gated the same way.
//!
//! Every operation resolves the registered checkout again. Historical remote
//! registrations cannot authorize a linked PR. A mismatch is skipped, and
//! origin is checked again before a GitHub observation can change story state.
//! Failures remain isolated per link and are reported by the whole call.

use std::collections::BTreeMap;

use crate::domain::github_remote::GithubRepo;
use crate::domain::pr_url::{PullRequestRef, parse_pr_url};
use crate::domain::{
    COMPLETION_STATE_SLUG, StoryEvent, SuperState, VERIFYING_STATE_SLUG, completion_state,
    has_children,
};
use crate::error::AppError;
use crate::github::api::{GithubApi, GithubApiFactory};
use crate::output::Response;
use crate::store::{ExpectedSeq, PrLink, ProjectId, ReadOps, Store, StoreError, StoryNo};

use super::github::RealGithubApiFactory;
use super::pr_link::PrLinkService;
use super::story::state_transition_events;
use super::verification::VERIFICATION_UNCERTIFIED_MERGE_PREFIX;
use super::{Ctx, append_and_fold, project_prefix, resolve_story};

impl<'ctx, S: Store> PrLinkService<'ctx, S> {
    /// Checks one story's (or, with `id: None`, every open story's) linked
    /// pull requests against GitHub, closing a story whose merged link asked
    /// to be closed on merge.
    ///
    /// # Errors
    ///
    /// Reports unavailable origins, gh authentication failures, and API errors.
    /// One failed observation does not prevent checking the remaining links.
    pub fn check(&self, id: Option<&str>) -> Result<Response, AppError> {
        run_check(self.ctx(), &RealGithubApiFactory, id)
    }
}

/// [`PrLinkService::check`]'s engine, taking the [`GithubApiFactory`] as a
/// parameter, so a test
/// can substitute `storyhook_test_support::FakeGithubApiFactory` without a
/// network. `PrLinkService::check` is the production wrapper, fixed to
/// [`RealGithubApiFactory`].
pub fn run_check<S: Store>(
    ctx: &Ctx<'_, S>,
    factory: &dyn GithubApiFactory,
    id: Option<&str>,
) -> Result<Response, AppError> {
    let project = ctx.project();
    let repository = super::github_repository::repository(ctx)?;
    let configured = [repository.identity().clone()];
    super::github_repository::poll_enabled(ctx)?;

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

    // Mandatory security control #2: a link whose (host, owner, repo) matches none
    // of the project's registered GitHub remotes **right now** is skipped,
    // not acted on — a remote may have been registered or unregistered
    // since the link was made, and a client is only ever built for a
    // repository this project currently claims (below).
    let mut skipped: Vec<String> = Vec::new();
    let matching: Vec<(StoryNo, PrLink, GithubRepo, PullRequestRef)> = candidates
        .into_iter()
        .filter_map(|(story_no, link)| {
            let reference = match parse_pr_url(&link.url) {
                Ok(reference) => reference,
                Err(_) => {
                    skipped.push(link.url.clone());
                    return None;
                }
            };
            let matched = configured.iter().find(|repo| {
                repo.host.eq_ignore_ascii_case(&reference.host)
                    && repo.owner.eq_ignore_ascii_case(&reference.owner)
                    && repo.repo.eq_ignore_ascii_case(&reference.repo)
            });
            if let Some(repo) = matched {
                Some((story_no, link, repo.clone(), reference))
            } else {
                skipped.push(link.url.clone());
                None
            }
        })
        .collect();

    let mut clients: BTreeMap<(String, String, String), Box<dyn GithubApi>> = BTreeMap::new();

    let mut merged: Vec<String> = Vec::new();
    let mut closed_without_merging: Vec<String> = Vec::new();
    let mut closed_stories: Vec<String> = Vec::new();
    let mut left_verifying: Vec<String> = Vec::new();
    // Stories whose pending landing intent leaves the observation to the
    // verifier's completion (SH-832 D7).
    let mut landing: Vec<String> = Vec::new();
    // Per-link GitHub API failures, isolated from one another: one
    // repository's error must not stop another repository's links in the
    // same run from being checked. Non-empty at the end turns this call
    // into an error — see the trailing check, and the module doc's SH-159
    // cross-reference.
    let mut errored: Vec<(String, String)> = Vec::new();
    let mut authentication_errors = 0;

    for (story_no, link, configured_repo, reference) in matching {
        let client = clients
            .entry((
                configured_repo.host.clone(),
                reference.owner.clone(),
                reference.repo.clone(),
            ))
            .or_insert_with(|| factory.build(repository.clone()));
        let status = match client.get_pull_request(reference.number) {
            Ok(status) => status,
            Err(err) => {
                if matches!(err, AppError::GithubAuth(_)) {
                    authentication_errors += 1;
                }
                errored.push((link.url.clone(), err.to_string()));
                continue;
            }
        };
        if super::github_repository::repository(ctx)?.identity() != repository.identity() {
            return Err(AppError::Validation("project origin changed while checking pull requests; no further lifecycle updates were applied".into()));
        }
        let now = ctx.now();

        if status.merged {
            let written = ctx.write_stories(|tx| {
                // A pending landing intent is the verifier's durable claim on
                // this merge: its completion records the merge itself, and
                // any write here would change the link the intent names,
                // which the store refuses before commit (SH-832 D7). Checked
                // inside this transaction so a new intent cannot slip in
                // between the check and the write.
                if landing_pending(tx, project, story_no)? {
                    return Ok(false);
                }
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
                if super::automations::permits_generation(tx, project, Some(row.head_global_seq))? && link.close_on_merge && !row.archived && !has_children(&row.snapshot) {
                    if row.state == VERIFYING_STATE_SLUG {
                        // Polling records the remote merge, but does not prove
                        // head ancestry in the current default branch. The
                        // verifier supplies that completion evidence (SH-857).
                        let id = story_no.to_id(&prefix);
                        events.push(StoryEvent::StoryCommentAdded {
                            at: now.clone(),
                            text: format!(
                                "{VERIFICATION_UNCERTIFIED_MERGE_PREFIX} pull request {} merged outside central verification. This story was verifying. Certification has not been checked by this poller. The story stays in `verifying`. The verifier checks whether its head is contained by the remote default branch and completes already-landed work. To complete it by hand, run `story move {id} done \"<reason>\"`.",
                                link.url
                            ),
                        });
                        left_verifying.push(id);
                    } else {
                        // The completion state by name, never "the first
                        // CLOSED state": `states` is a BTreeMap, so that
                        // search answered `closed` — abandonment — on every
                        // default catalog (SH-652).
                        let completion =
                            completion_state(&tx.states(project)?).ok_or_else(|| {
                                StoreError::Invariant(format!(
                                    "project has no CLOSED `{COMPLETION_STATE_SLUG}` state"
                                ))
                            })?;
                        events.extend(state_transition_events(
                            &completion,
                            row.awaiting.is_some(),
                            &now,
                            Vec::new(),
                        ));
                        closed_stories.push(story_no.to_id(&prefix));
                    }
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
                if row.superstate == SuperState::Open
                    && events
                        .iter()
                        .any(|event| matches!(event, StoryEvent::StoryClosedAndArchived { .. }))
                {
                    super::relation::retract_closed_blocker_edges(
                        tx,
                        project,
                        story_no,
                        &prefix,
                        &states,
                        &now,
                        ctx.provenance(),
                    )?;
                }
                Ok(true)
            })?;
            if written {
                merged.push(link.url.clone());
            } else {
                landing.push(story_no.to_id(&prefix));
            }
        } else if status.state == "closed" {
            let written = ctx.write_stories(|tx| {
                if landing_pending(tx, project, story_no)? {
                    return Ok(false);
                }
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
                Ok(true)
            })?;
            if written {
                closed_without_merging.push(link.url.clone());
            } else {
                landing.push(story_no.to_id(&prefix));
            }
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
    if !left_verifying.is_empty() {
        message.push_str(&format!(
            "\nleft verifying (merged without a verdict): {}",
            left_verifying.join(", ")
        ));
    }
    if !landing.is_empty() {
        message.push_str(&format!(
            "\nlanding in progress (the verifier completes it): {}",
            landing.join(", ")
        ));
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
    Err(if authentication_errors == errored.len() {
        AppError::GithubAuth(message)
    } else {
        AppError::GithubApi(message)
    })
}

/// Whether `story` has a pending landing intent (single story or batch
/// member): its merge outcome is the verifier's to record.
fn landing_pending(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<bool, StoreError> {
    Ok(tx
        .landing_intents()?
        .iter()
        .any(|intent| intent.project == project && intent.story == story))
}
