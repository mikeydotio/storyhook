//! The centralized release-gate queue (SH-521).
//!
//! Queue membership is a story fact, not a second job record: every OPEN story
//! in required state [`VERIFYING_STATE`] is recoverable work after a daemon
//! restart. One daemon worker asks this service for the first candidate, so a
//! crashed attempt is retried from the same durable source of truth.

use std::path::PathBuf;

use crate::domain::pr_url::parse_pr_url;
use crate::domain::{
    COMPLETION_STATE_SLUG, Priority, StateDef, StoryCleanupLease, StoryEvent, SubmittedPullRequest,
    SuperState, VERIFYING_STATE_SLUG, completion_state,
};
use crate::error::AppError;
use crate::store::{
    ExpectedSeq, GlobalSeq, PrLink, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryQuery,
    StoryRow, VerificationFailureDisposition, VerificationIncident, WriteOps,
};

use super::gate_progress::GATE_PROGRESS_PREFIX;
use super::story::{append_state_transition, state_transition_events};
use super::{Ctx, append_and_fold, project_prefix, relation, resolve_story};

/// The required OPEN state that hands a published PR to the verifier.
pub const VERIFYING_STATE: &str = VERIFYING_STATE_SLUG;

/// The required OPEN state a story is returned to when verification hands it
/// back to its agent (conflict, red, or an invalid submission). Named once so
/// the Full Auto reconciler recognises "returned for repair" by the same
/// spelling the verifier writes (SH-650, `returned_for_repair`).
pub const RETURNED_STATE: &str = "in-progress";

/// Whether the story's own state history ends with the verifier's return:
/// its latest `StoryStateChanged` is [`RETURNED_STATE`] and the one before it
/// is [`VERIFYING_STATE`], with no state change since (SH-650).
///
/// This is the store-derived fact the engine reads instead of a lane mark the
/// verifier would have to write: between `record_generation_returned` and the
/// respawned pane coming alive, the story is `in-progress` with no `awaiting`
/// and a dead window, and a reconciler that took the dead window as evidence
/// would quarantine the lane and strike the breaker for the very remediation
/// the verifier is delivering. The fact ends, by construction, at the story's
/// next state change — the agent resubmitting to `verifying`, or a person
/// moving it — and is overridden earlier by `awaiting` (the verifier's own
/// refusal to re-dispatch), which the engine classifies ahead of the window.
pub fn returned_for_repair(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<bool, StoreError> {
    let events = tx.events_for(project, story)?;
    let mut changes = events.iter().rev().filter_map(|event| match event.known() {
        Some(StoryEvent::StoryStateChanged { state, .. }) => Some(state.as_str()),
        _ => None,
    });
    Ok(changes.next() == Some(RETURNED_STATE) && changes.next() == Some(VERIFYING_STATE))
}

/// Durable comment prefix proving the centralized release gate landed a PR.
pub const VERIFICATION_GREEN_PREFIX: &str = "CENTRAL VERIFICATION GREEN —";

/// Durable comment prefix proving post-merge resources were reclaimed.
pub const VERIFICATION_CLEANUP_COMPLETE_PREFIX: &str = "CENTRAL VERIFICATION CLEANUP COMPLETE —";

/// Durable comment prefix for verifier infrastructure failures.
pub(crate) const VERIFICATION_INFRASTRUCTURE_PREFIX: &str = "CENTRAL VERIFICATION INFRASTRUCTURE —";

/// Durable comment prefix recording that the verifier pushed a leased branch
/// and opened or adopted its pull request (SH-647). One marked comment per
/// generation: a resubmission that moves the branch replaces it.
pub const VERIFICATION_SUBMITTED_PREFIX: &str = "CENTRAL VERIFICATION SUBMITTED —";

/// The marker every withdrawal record starts with (SH-692): an attempt the
/// verifier cancelled because its generation lost authority — the story left
/// `verifying`, was resubmitted, or was blocked — judged nothing, and the
/// story says so instead of ending on a PROGRESS comment that reads
/// "running" for ever.
pub const VERIFICATION_WITHDRAWN_PREFIX: &str = "CENTRAL VERIFICATION WITHDRAWN —";

/// The marker a hand completion of a `verifying` story carries (SH-692): the
/// operator's stated reason for overriding central verification, recorded in
/// the same transaction as the move to `done`. Its presence is what lets the
/// completion through [`refuse_uncertified_completion`] and what makes the
/// story reap-eligible once its pull request is recorded merged.
pub const VERIFICATION_OVERRIDDEN_PREFIX: &str = "CENTRAL VERIFICATION OVERRIDDEN —";

/// The marker the GitHub poller leaves on a `verifying` story whose pull
/// request merged outside central verification (SH-692). The poller records
/// the fact and never completes the story: nothing certified the merge tree.
pub const VERIFICATION_UNCERTIFIED_MERGE_PREFIX: &str = "CENTRAL VERIFICATION UNCERTIFIED MERGE —";

/// What a caller is told when it tries to complete a `verifying` story with
/// no verdict and no stated reason (SH-692). Names both ways out.
pub(crate) fn override_refusal(id: &str) -> String {
    format!(
        "story `{id}` is under central verification; completing it by hand overrides the verifier and requires a reason: `story move {id} done \"<why>\"` records it as `{VERIFICATION_OVERRIDDEN_PREFIX} <why>` and the verifier withdraws its running attempt. To hand the story back without completing it, `story move {id} in-progress`."
    )
}

/// The one rule every door shares (SH-692): a story leaves `verifying` for
/// the completion state only with a verdict — the verifier's own GREEN,
/// written in the same batch, or one posted for this generation — or with an
/// operator's OVERRIDDEN reason in the batch. Called from
/// [`super::append_and_fold`], the write path every service funnels through,
/// so `story move`, `story set --state`, the dashboard's move and PATCH, epic
/// materialisation and state-catalog migration cannot disagree about it.
/// Costs one row read, and one events read only on the transitions it judges.
pub(crate) fn refuse_uncertified_completion(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    events: &[StoryEvent],
) -> Result<(), AppError> {
    let completes = events.iter().any(|event| {
        matches!(event, StoryEvent::StoryStateChanged { state, .. } if state == COMPLETION_STATE_SLUG)
    });
    if !completes {
        return Ok(());
    }
    let Some(row) = tx.story(project, story)? else {
        return Ok(());
    };
    if row.state != VERIFYING_STATE {
        return Ok(());
    }
    let carries_verdict = |text: &str| {
        text.starts_with(VERIFICATION_GREEN_PREFIX)
            || text.starts_with(VERIFICATION_OVERRIDDEN_PREFIX)
    };
    if events.iter().any(|event| {
        matches!(event, StoryEvent::StoryCommentAdded { text, .. } if carries_verdict(text))
    }) {
        return Ok(());
    }
    if certified_for_current_stay(tx, project, story, &row)? {
        return Ok(());
    }
    Err(AppError::Validation(override_refusal(&row.snapshot.id)))
}

/// Whether `row` carries a GREEN verdict for its current stay in
/// `verifying` (SH-692). A GREEN from an earlier generation certified an
/// earlier tree, not the one a completion now would record as landed, so
/// only a verdict at or after the latest entry into `verifying` counts.
/// Shared by [`refuse_uncertified_completion`] and `set_state`, so the door
/// and the backstop cannot disagree about what needs no override.
pub(crate) fn certified_for_current_stay(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    row: &StoryRow,
) -> Result<bool, StoreError> {
    let entered_at = verifying_entry(tx, project, story)?.map(|(at, _)| at);
    Ok(row.snapshot.comments.iter().any(|comment| {
        comment.text.starts_with(VERIFICATION_GREEN_PREFIX)
            && entered_at
                .as_deref()
                .is_none_or(|entered| comment.at.as_str() >= entered)
    }))
}

/// Result of a write whose authority belongs to one verification generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationWrite<T> {
    /// The candidate remained current and the write committed.
    Applied(T),
    /// A later state transition superseded the candidate before the write.
    Superseded,
}

impl<T> GenerationWrite<T> {
    /// Maps the applied value, leaving a superseded write superseded.
    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> GenerationWrite<U> {
        match self {
            GenerationWrite::Applied(value) => GenerationWrite::Applied(f(value)),
            GenerationWrite::Superseded => GenerationWrite::Superseded,
        }
    }
}

/// A malformed verification submission that must return to its author.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerificationProblem {
    /// The project has no registered checkout in which to run its gate.
    MissingCheckout,
    /// No open, close-on-merge PR is linked to the story.
    MissingPullRequest,
    /// More than one open, close-on-merge PR makes the submission ambiguous.
    MultiplePullRequests(Vec<String>),
    /// The linked PR no longer belongs to a registered GitHub repository.
    UnregisteredPullRequest {
        /// The stale or cross-repository link.
        url: String,
        /// GitHub repositories currently registered for the project.
        registered: Vec<String>,
    },
}

impl VerificationProblem {
    /// A durable diagnosis suitable for both a story comment and agent prompt.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::MissingCheckout => "verification cannot run because this project has no registered checkout; run `story project link checkout <path>` from an operator session".to_string(),
            Self::MissingPullRequest => "verification needs exactly one open close-on-merge pull request linked with `story link-pr`; none is linked".to_string(),
            Self::MultiplePullRequests(urls) => format!(
                "verification needs exactly one open close-on-merge pull request; found {}: {}",
                urls.len(),
                urls.join(", ")
            ),
            Self::UnregisteredPullRequest { url, registered } => format!(
                "verification refuses linked pull request `{url}` because it does not match a currently registered GitHub repository ({})",
                if registered.is_empty() {
                    "none registered".to_string()
                } else {
                    registered.join(", ")
                }
            ),
        }
    }
}

/// One story selected for centralized verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationCandidate {
    /// Store identity of the owning project.
    pub project: ProjectId,
    /// Stable project slug used by helper subprocesses.
    pub project_slug: String,
    /// Project story id, including its prefix.
    pub story_id: String,
    /// Story title for diagnostics.
    pub title: String,
    /// Stored priority that ordered the queue.
    pub priority: Priority,
    /// Creation timestamp used to break equal-priority ties during cleanup.
    pub created_at: String,
    /// When this story most recently entered [`VERIFYING_STATE`], read from
    /// its own `StoryStateChanged` history rather than `updated_at` (SH-524):
    /// the progress checklist rewrites `updated_at` on every publish, which
    /// would make the story's own recency field lie about queue wait time.
    /// `None` for a [`Self::pull_request`] read that predates this field
    /// (`next_cleanup`'s completed-story pass, where wait time is moot) or
    /// for the vanishingly unlikely case no such event survives.
    /// Breaks equal-priority verification ties, oldest entry first. Missing
    /// timestamps follow known timestamps, without inventing a queue age.
    pub verifying_since: Option<String>,
    /// Exact change-feed position of the latest transition into
    /// [`VERIFYING_STATE`]. Unlike a story id or timestamp, this cannot be
    /// reused by a later submission of the same story, so verifier ownership
    /// and progress journals can reject stale attempts (SH-549).
    pub verifying_generation: Option<GlobalSeq>,
    /// Latest durable block edge at admission; a transient hold revokes this attempt.
    pub blocking_revision: Option<i64>,
    /// Registered checkout where the repository-side verifier runs.
    pub checkout: PathBuf,
    /// Exact disposable resources owned by this verification generation.
    ///
    /// `None` denotes a legacy or manual submission. Verification may still
    /// merge it, but centralized cleanup must remain explicitly required.
    pub cleanup_lease: Option<StoryCleanupLease>,
    /// The single submitted PR, or why the submission is ambiguous.
    pub pull_request: Result<PrLink, VerificationProblem>,
}

/// Store-backed verification queue and completion writer.
pub struct VerificationQueue<'a, S: Store> {
    store: &'a S,
}

/// Acknowledges exactly the halted verification incident `incident_id` for the
/// project `ctx` selects, without changing manual admission permission (SH-668).
///
/// Shares transactional validation with `POST .../verification/ack` so
/// `story verifier ack <incident-id>` (SH-666) cannot drift from it (SH-136):
/// the id must name the incident that is *current*, still
/// halted rather than retrying, and this project's. A reader of a stale
/// comment therefore cannot acknowledge a newer incident by accident, and an
/// acknowledgement retries nothing by itself: the verifier's next tick does,
/// provided manual admission is enabled.
pub fn acknowledge_verification_incident<S: Store>(
    ctx: &Ctx<'_, S>,
    incident_id: &str,
) -> Result<VerificationIncident, AppError> {
    Ok(ctx.store().write(|tx| {
        super::verification_control::acknowledge_in_transaction(tx, ctx.project(), incident_id)
    })?)
}

impl<'a, S: Store> VerificationQueue<'a, S> {
    /// Creates a queue over every project in one daemon store.
    #[must_use]
    pub fn new(store: &'a S) -> Self {
        Self { store }
    }

    /// Returns the highest-priority submitted story across every project.
    ///
    /// Its first element is what [`Self::ordered`] would also return first;
    /// kept as its own call so a caller that only needs the head does not pay
    /// for building the queued rest of the list under a busy daemon.
    pub fn next(&self) -> Result<Option<VerificationCandidate>, AppError> {
        Ok(self.ordered()?.into_iter().next())
    }

    /// Returns every submitted story across every project, in one global
    /// order (SH-651): priority, then oldest current verification entry, then
    /// project/story identity. Each project's worker drains [`Self::ordered_for`]; this is
    /// the cross-project view.
    pub fn ordered(&self) -> Result<Vec<VerificationCandidate>, AppError> {
        Ok(self.store.read(|tx| ordered_candidates(tx))?)
    }

    /// Returns one project's submitted stories in the exact order its worker
    /// drains them (SH-648). A queued candidate's position and wait are
    /// computed from this list, never re-derived from a second query that
    /// could race the one the worker itself used.
    pub fn ordered_for(&self, project: ProjectId) -> Result<Vec<VerificationCandidate>, AppError> {
        Ok(self.store.read(|tx| ordered_candidates_for(tx, project))?)
    }

    /// Returns this story's current submitted generation, if it still has one.
    pub(crate) fn current_for(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<Option<VerificationCandidate>, AppError> {
        Ok(self.store.read(|tx| {
            Ok(ordered_candidates_for(tx, candidate.project)?
                .into_iter()
                .find(|current| current.story_id == candidate.story_id))
        })?)
    }

    /// Records completed execution and optional cleanup incident in one transaction.
    /// A merge URL is supplied only after the guarded merge has actually landed.
    pub(crate) fn record_generation_completed(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        pull_request_url: Option<&str>,
        verdict_comment: &str,
        cleanup_detail: Option<&str>,
    ) -> Result<GenerationWrite<Option<VerificationIncident>>, AppError> {
        let project = candidate.project;
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(&*tx, &row, candidate)? {
                return Ok(GenerationWrite::Superseded);
            }
            let mut events = Vec::new();
            let already_recorded = tx.events_for(project, story_no)?.iter().any(|event| {
                candidate.verifying_generation.is_none_or(|generation| event.global_seq > generation)
                    && matches!(event.known(), Some(StoryEvent::StoryCommentAdded { text, .. }) if text == verdict_comment)
            });
            if !already_recorded {
                events.push(StoryEvent::StoryCommentAdded {
                    at: now.clone(), text: verdict_comment.to_string(),
                });
            }
            let incident = if let Some(detail) = cleanup_detail {
                let generation = candidate.verifying_generation.ok_or_else(|| {
                    StoreError::Corrupt(format!("{} has no verification generation", candidate.story_id))
                })?;
                let incident_id = format!("{}:{}", project.get(), generation.get());
                let previous = tx.verification_incident(project)?
                    .filter(|incident| incident.incident_id == incident_id);
                let incident = VerificationIncident {
                    incident_id, project, story: story_no, generation,
                    disposition: VerificationFailureDisposition::Permanent, halted: true,
                    attempts: previous.as_ref().map_or(1, |incident| incident.attempts),
                    first_failed_at: previous.as_ref().map_or_else(|| now.clone(), |incident| incident.first_failed_at.clone()),
                    last_failed_at: previous.as_ref().map_or_else(|| now.clone(), |incident| incident.last_failed_at.clone()),
                    detail: detail.to_string(),
                };
                let body = format!(
                    "{VERIFICATION_INFRASTRUCTURE_PREFIX} HALTED\n\nThe completed verdict above remains valid. Post-gate cleanup failed; this halt stops the verifier's whole queue. {} Establish writer quiescence and repair retained resources before releasing the queue with: story verifier ack {}\n\n{detail}",
                    if pull_request_url.is_some() { "The PR landed; automatic reaping is suspended." } else { "The story remains verifying; remediation dispatch and landing are suspended." },
                    incident.incident_id,
                );
                events.extend(marked_comment_events(&row, VERIFICATION_INFRASTRUCTURE_PREFIX, &body, &now));
                Some(incident)
            } else { None };
            let states = tx.state_map(project)?;
            if let Some(url) = pull_request_url {
                let linked = tx.open_pr_links_for_story(project, story_no)?.into_iter()
                    .any(|link| link.close_on_merge && link.url == url);
                if !linked {
                    return Err(AppError::Validation(format!(
                        "story `{}` no longer links submitted pull request `{url}`", candidate.story_id,
                    )).into());
                }
                let done = completion_state_or_refuse(&tx.states(project)?)?;
                events.push(StoryEvent::StoryPrMerged { at: now.clone(), url: url.to_string() });
                append_state_transition(tx, project, story_no, &row, &prefix, &states,
                    &done, &now, events, ctx.provenance())?;
            } else if !events.is_empty() {
                append_and_fold(tx, project, story_no, &prefix, &states,
                    ExpectedSeq::Exact(row.head_seq), &events, ctx.provenance())?;
            }
            if let Some(incident) = &incident {
                tx.put_verification_incident(incident)?;
            } else {
                clear_candidate_incident(tx, candidate)?;
            }
            Ok(GenerationWrite::Applied(incident))
        })?)
    }

    /// Atomically records a submission the verifier just made (SH-647): the
    /// pull request link, `close_on_merge`, and one marked SUBMITTED comment,
    /// only while `candidate` is current. Hands back the open link as the
    /// store now folds it, so the caller proceeds on a store fact rather than
    /// on a `PrLink` it assembled by hand.
    ///
    /// The cross-repository rule is `PrLinkService::link`'s: a project with
    /// registered GitHub origins accepts only a pull request on one of them;
    /// a project with none registered has nothing to check against. Re-linking
    /// a URL the story already links upserts, which is what makes recording
    /// the adopted pull request on every generation idempotent.
    pub(crate) fn record_generation_submitted(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        pull_request: &SubmittedPullRequest,
    ) -> Result<GenerationWrite<PrLink>, AppError> {
        let reference = parse_pr_url(&pull_request.url)?;
        let branch = candidate
            .cleanup_lease
            .as_ref()
            .map(|lease| lease.branch.clone())
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "story `{}` has no cleanup lease; nothing was submitted on its behalf",
                    candidate.story_id
                ))
            })?;
        let project = candidate.project;
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(&*tx, &row, candidate)? {
                return Ok(GenerationWrite::Superseded);
            }
            let registered =
                super::pr_link::github_repos_from_remotes(&tx.project_remotes(project)?);
            if !registered.is_empty()
                && !registered.iter().any(|repo| {
                    repo.host.eq_ignore_ascii_case(&reference.host)
                        && repo.owner.eq_ignore_ascii_case(&reference.owner)
                        && repo.repo.eq_ignore_ascii_case(&reference.repo)
                })
            {
                return Err(AppError::Validation(format!(
                    "the verifier opened pull request `{}` on a repository this project has not registered ({}); refusing to link it",
                    pull_request.url,
                    registered
                        .iter()
                        .map(|repo| format!("{}/{}/{}", repo.host, repo.owner, repo.repo))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .into());
            }
            let body = format!(
                "{VERIFICATION_SUBMITTED_PREFIX} `{branch}` is on origin at {}; {} pull request {}.",
                pull_request.head_oid,
                if pull_request.adopted {
                    "adopted open"
                } else {
                    "opened"
                },
                pull_request.url
            );
            let mut events = vec![StoryEvent::StoryPrLinked {
                at: now.clone(),
                url: pull_request.url.clone(),
                owner: reference.owner.clone(),
                repo: reference.repo.clone(),
                number: reference.number,
                close_on_merge: true,
            }];
            events.extend(marked_comment_events(
                &row,
                VERIFICATION_SUBMITTED_PREFIX,
                &body,
                &now,
            ));
            let states = tx.state_map(project)?;
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
            let link = tx
                .open_pr_links_for_story(project, story_no)?
                .into_iter()
                .find(|link| link.close_on_merge && link.url == pull_request.url)
                .ok_or_else(|| {
                    StoreError::Corrupt(format!(
                        "pull request `{}` was just linked to `{}` and does not read back open",
                        pull_request.url, candidate.story_id
                    ))
                })?;
            Ok(GenerationWrite::Applied(link))
        })?)
    }

    /// Atomically records a diagnosis and returns only the current generation.
    pub(crate) fn record_generation_returned(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        diagnosis: &str,
    ) -> Result<GenerationWrite<()>, AppError> {
        let project = candidate.project;
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(&*tx, &row, candidate)? {
                return Ok(GenerationWrite::Superseded);
            }
            let states = tx.state_map(project)?;
            let target = states.get(RETURNED_STATE).cloned().ok_or_else(|| {
                AppError::Validation(format!(
                    "project has no required OPEN `{RETURNED_STATE}` state; run `story doctor --fix`"
                ))
            })?;
            clear_candidate_incident(tx, candidate)?;
            append_state_transition(
                tx,
                project,
                story_no,
                &row,
                &prefix,
                &states,
                &target,
                &now,
                vec![StoryEvent::StoryCommentAdded {
                    at: now.clone(),
                    text: diagnosis.to_string(),
                }],
                ctx.provenance(),
            )?;
            Ok(GenerationWrite::Applied(()))
        })?)
    }

    /// Records a failed remediation delivery only if no later submission has
    /// replaced the generation that was returned for repair.
    pub(crate) fn set_generation_awaiting(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        reason: &str,
    ) -> Result<GenerationWrite<()>, AppError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(AppError::Validation(
                "awaiting reason must not be empty".to_string(),
            ));
        }
        let project = candidate.project;
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if row.state != "in-progress" || !candidate_is_latest_generation(&*tx, &row, candidate)?
            {
                return Ok(GenerationWrite::Superseded);
            }
            let states = tx.state_map(project)?;
            append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryAwaitingSet {
                    at: now,
                    awaiting: reason.to_string(),
                }],
                ctx.provenance(),
            )?;
            Ok(GenerationWrite::Applied(()))
        })?)
    }

    /// Atomically records an infrastructure incident for the current generation.
    pub(crate) fn record_generation_incident(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        disposition: VerificationFailureDisposition,
        detail: &str,
        retry_attempts: u32,
    ) -> Result<GenerationWrite<VerificationIncident>, AppError> {
        let generation = candidate.verifying_generation.ok_or_else(|| {
            AppError::Storage(format!(
                "{} has no verification generation",
                candidate.story_id
            ))
        })?;
        let project = candidate.project;
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(&*tx, &row, candidate)? {
                return Ok(GenerationWrite::Superseded);
            }
            let incident_id = format!("{}:{}", candidate.project.get(), generation.get());
            let mut incident = tx
                .verification_incident(project)?
                .filter(|current| current.incident_id == incident_id)
                .unwrap_or(VerificationIncident {
                    incident_id,
                    project: candidate.project,
                    story: story_no,
                    generation,
                    disposition,
                    halted: false,
                    attempts: 0,
                    detail: String::new(),
                    first_failed_at: now.clone(),
                    last_failed_at: now.clone(),
                });
            incident.attempts = incident.attempts.saturating_add(1);
            incident.disposition = disposition;
            incident.detail = detail.to_string();
            incident.last_failed_at = now.clone();
            incident.halted = disposition == VerificationFailureDisposition::Permanent
                || incident.attempts >= retry_attempts;
            let state = if incident.halted {
                "HALTED"
            } else {
                "RETRYING"
            };
            // What the reader must not conclude: that this story is the
            // cause, or that the verifier is still serving anyone. An
            // infrastructure incident is the verifier's own, it stops the
            // whole queue, and the reader is in a terminal — so the halted
            // form names the exact command that releases it (SH-666).
            let consequence = if incident.halted {
                format!(
                    "This halt stops the verifier's whole queue. No story is at fault: the verifier itself could not run, and {} is only where the failure was first hit. Fix the cause below, then release the queue with: story verifier ack {}",
                    candidate.story_id, incident.incident_id
                )
            } else {
                "The verifier is retrying on its own; every story behind this one waits until it recovers or halts.".to_string()
            };
            let body = format!(
                "{VERIFICATION_INFRASTRUCTURE_PREFIX} {state}\n\nAttempt {} of {retry_attempts}. First failure: {}. Latest attempt: {}.\nThe story remains verifying; its code was not classified red.\n{consequence}\n\n{}",
                incident.attempts,
                incident.first_failed_at,
                incident.last_failed_at,
                incident.detail
            );
            let events = marked_comment_events(
                &row,
                VERIFICATION_INFRASTRUCTURE_PREFIX,
                &body,
                &now,
            );
            if !events.is_empty() {
                let states = tx.state_map(project)?;
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
            }
            tx.put_verification_incident(&incident)?;
            Ok(GenerationWrite::Applied(incident))
        })?)
    }

    /// Records that `candidate`'s attempt was withdrawn (SH-692): retracts the
    /// generation's last PROGRESS comment, whose "running" would otherwise be
    /// the story's final word, and appends `body`. Deliberately NOT
    /// generation-guarded — a withdrawal is by definition written after the
    /// generation lost authority — and written to the story in whatever state
    /// it is now in, closed included, the way `story comment` is (SH-261): the
    /// record is an observation about the attempt, not a change to the story.
    /// Idempotent on an identical body. Returns whether anything was written.
    pub(crate) fn record_generation_withdrawn(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        body: &str,
    ) -> Result<bool, AppError> {
        let project = candidate.project;
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if row
                .snapshot
                .comments
                .iter()
                .any(|comment| comment.text == body)
            {
                return Ok(false);
            }
            let mut events = Vec::new();
            if let Some(progress) = row
                .snapshot
                .comments
                .iter()
                .rev()
                .find(|comment| comment.text.starts_with(GATE_PROGRESS_PREFIX))
            {
                events.push(StoryEvent::StoryCommentRetracted {
                    at: now.clone(),
                    comment_at: progress.at.clone(),
                    text: progress.text.clone(),
                });
            }
            events.push(StoryEvent::StoryCommentAdded {
                at: now.clone(),
                text: body.to_string(),
            });
            let states = tx.state_map(project)?;
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
            Ok(true)
        })?)
    }

    /// Rewrites a marked comment only while `candidate` remains current.
    pub(crate) fn upsert_generation_comment(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        marker: &str,
        body: &str,
    ) -> Result<GenerationWrite<bool>, AppError> {
        let project = candidate.project;
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(&*tx, &row, candidate)? {
                return Ok(GenerationWrite::Superseded);
            }
            let events = marked_comment_events(&row, marker, body, &now);
            if events.is_empty() {
                return Ok(GenerationWrite::Applied(false));
            }
            let states = tx.state_map(project)?;
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
            Ok(GenerationWrite::Applied(true))
        })?)
    }

    /// Returns a completed story, in any project, whose post-merge resources
    /// still need reap. Each project's worker asks [`Self::next_cleanup_for`];
    /// this is the cross-project view.
    pub fn next_cleanup(&self) -> Result<Option<VerificationCandidate>, AppError> {
        Ok(self.store.read(|tx| {
            let mut candidates = Vec::new();
            for project in tx.projects()? {
                candidates.extend(cleanup_candidates_for(tx, project.id)?);
            }
            sort_cleanup_candidates(&mut candidates);
            Ok(candidates.into_iter().next())
        })?)
    }

    /// Returns one project's completed story whose post-merge resources still
    /// need reap.
    ///
    /// Active verification is intentionally queried separately and first by
    /// the daemon so a transient cleanup fault cannot starve the release gate.
    pub fn next_cleanup_for(
        &self,
        project: ProjectId,
    ) -> Result<Option<VerificationCandidate>, AppError> {
        Ok(self.store.read(|tx| {
            let mut candidates = cleanup_candidates_for(tx, project)?;
            sort_cleanup_candidates(&mut candidates);
            Ok(candidates.into_iter().next())
        })?)
    }
}

/// One project's `done` stories that passed central verification — or were
/// completed over it by an operator's recorded override AND whose pull request
/// is recorded merged (SH-692) — and have not yet been reaped (SH-648: the
/// cleanup pass is per project, like the queue it follows).
///
/// The override alone is not enough: a reap deletes the branch and the
/// worktree, and an overridden story whose pull request never merged may
/// still be the only place that work lives. The merged link is the evidence
/// that it landed, the same evidence the GREEN path carries implicitly.
fn cleanup_candidates_for(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<Vec<VerificationCandidate>, StoreError> {
    let mut candidates = Vec::new();
    if let Some(project) = tx.project(project)? {
        let checkout = tx.checkout_path(project.id)?.unwrap_or_default();
        let links = tx.pr_links(project.id)?;
        let rows = tx.stories(project.id, &StoryQuery::all().state(COMPLETION_STATE_SLUG))?;
        for row in rows {
            let passed = row
                .snapshot
                .comments
                .iter()
                .any(|comment| comment.text.starts_with(VERIFICATION_GREEN_PREFIX));
            let overridden = row
                .snapshot
                .comments
                .iter()
                .any(|comment| comment.text.starts_with(VERIFICATION_OVERRIDDEN_PREFIX));
            let reaped = row.snapshot.comments.iter().any(|comment| {
                comment
                    .text
                    .starts_with(VERIFICATION_CLEANUP_COMPLETE_PREFIX)
            });
            let pull_request = links
                .iter()
                .find(|(story_no, link)| {
                    *story_no == row.story_no && link.close_on_merge && link.status == "merged"
                })
                .map(|(_, link)| link.clone())
                .ok_or(VerificationProblem::MissingPullRequest);
            let landed_by_override = overridden && pull_request.is_ok();
            if !(passed || landed_by_override) || reaped {
                continue;
            }
            candidates.push(VerificationCandidate {
                project: project.id,
                project_slug: project.slug.clone(),
                story_id: row.story_no.to_id(&project.prefix),
                title: row.title,
                priority: row.priority,
                created_at: row.created_at,
                // The cleanup pass runs over stories already `done` —
                // there is no queue wait left to report.
                verifying_since: None,
                verifying_generation: None,
                blocking_revision: None,
                checkout: checkout.clone(),
                cleanup_lease: latest_cleanup_lease(tx, project.id, row.story_no)?,
                pull_request,
            });
        }
    }
    Ok(candidates)
}

impl<S: Store> VerificationQueue<'_, S> {
    /// Records the verifier-observed merge and closes the submitted story.
    ///
    /// The exact PR URL is checked inside the write transaction. A stale
    /// worker can therefore never close a story for a PR its author replaced.
    pub fn record_merged(
        &self,
        ctx: &Ctx<'_, S>,
        story_id: &str,
        pull_request_url: &str,
    ) -> Result<(), AppError> {
        let project = ctx.project();
        let now = ctx.now();
        ctx.write_stories(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, story_id)?;
            if row.superstate == SuperState::Closed {
                return Ok(());
            }
            if row.state != VERIFYING_STATE {
                return Err(AppError::StateConflict(
                    VERIFYING_STATE.to_string(),
                    row.state.clone(),
                )
                .into());
            }
            let linked = tx
                .open_pr_links_for_story(project, story_no)?
                .into_iter()
                .any(|link| link.close_on_merge && link.url == pull_request_url);
            if !linked {
                return Err(AppError::Validation(format!(
                    "story `{story_id}` no longer links submitted pull request `{pull_request_url}`"
                ))
                .into());
            }
            let done = completion_state_or_refuse(&tx.states(project)?)?;
            let states = tx.state_map(project)?;
            let mut events = vec![StoryEvent::StoryPrMerged {
                at: now.clone(),
                url: pull_request_url.to_string(),
            }];
            events.extend(state_transition_events(
                &done,
                row.awaiting.is_some(),
                &now,
                Vec::new(),
            ));
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
            relation::retract_closed_blocker_edges(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                &now,
                ctx.provenance(),
            )?;
            Ok(())
        })?;
        Ok(())
    }
}

/// The state a green merge lands a story in, or the refusal that names the
/// repair (SH-652).
///
/// One door for both writers so the verifier cannot disagree with itself, and
/// a refusal rather than a fallback: a catalog with no CLOSED `done` is below
/// the SH-125 floor, and writing verified work into any other CLOSED state
/// would record the wrong business outcome — the reason SH-521 chose the
/// required slug over "whichever CLOSED state sorts first" in the first place.
fn completion_state_or_refuse(states: &[StateDef]) -> Result<StateDef, AppError> {
    completion_state(states).ok_or_else(|| {
        AppError::Validation(format!(
            "project has no required CLOSED `{COMPLETION_STATE_SLUG}` state; run `story doctor --fix`"
        ))
    })
}

fn candidate_is_current(
    tx: &impl ReadOps,
    row: &StoryRow,
    candidate: &VerificationCandidate,
) -> Result<bool, StoreError> {
    let stories = super::query::story_map(tx, candidate.project)?;
    if row.state != VERIFYING_STATE
        || crate::domain::is_blocked(&row.snapshot, &stories)
        || blocking_revision(tx, candidate.project, row.story_no)? != candidate.blocking_revision
    {
        return Ok(false);
    }
    candidate_is_latest_generation(tx, row, candidate)
}

fn blocking_revision(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<i64>, StoreError> {
    Ok(tx
        .block_deliveries(project)?
        .into_iter()
        .rev()
        .find(|d| d.story == story && d.action == crate::store::BlockAction::Interrupt)
        .map(|d| d.id))
}

fn candidate_is_latest_generation(
    tx: &impl ReadOps,
    row: &StoryRow,
    candidate: &VerificationCandidate,
) -> Result<bool, StoreError> {
    Ok(
        verifying_entry(tx, candidate.project, row.story_no)?.map(|(_, generation)| generation)
            == candidate.verifying_generation,
    )
}

fn clear_candidate_incident(
    tx: &mut impl WriteOps,
    candidate: &VerificationCandidate,
) -> Result<(), StoreError> {
    if let Some(incident) = tx
        .verification_incident(candidate.project)?
        .filter(|incident| candidate.verifying_generation == Some(incident.generation))
    {
        tx.clear_verification_incident(&incident.incident_id)?;
    }
    Ok(())
}

fn marked_comment_events(row: &StoryRow, marker: &str, body: &str, now: &str) -> Vec<StoryEvent> {
    let existing = row
        .snapshot
        .comments
        .iter()
        .rev()
        .find(|comment| comment.text.starts_with(marker));
    if existing.is_some_and(|comment| comment.text == body) {
        return Vec::new();
    }
    let mut events = Vec::new();
    if let Some(existing) = existing {
        events.push(StoryEvent::StoryCommentRetracted {
            at: now.to_string(),
            comment_at: existing.at.clone(),
            text: existing.text.clone(),
        });
    }
    events.push(StoryEvent::StoryCommentAdded {
        at: now.to_string(),
        text: body.to_string(),
    });
    events
}

/// The timestamp `story` most recently entered [`VERIFYING_STATE`], read from
/// the story's own `StoryStateChanged` history. `None` for the vanishingly
/// unlikely case no such event survives (SH-372: absence states nothing —
/// this is not asserted as an invariant, since a caller degrading to "wait
/// unknown" is safer than a queue read that can fail for one odd story).
pub(crate) fn verifying_entry(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<(String, GlobalSeq)>, StoreError> {
    let events = tx.events_for(project, story)?;
    Ok(events.iter().rev().find_map(|event| match event.known() {
        Some(StoryEvent::StoryStateChanged { at, state }) if state == VERIFYING_STATE => {
            Some((at.clone(), event.global_seq))
        }
        _ => None,
    }))
}

/// Every submitted story across every project, read from an existing
/// transaction, in one global order. Kept for the surfaces that look across
/// projects; each project's own worker and dashboard use
/// [`ordered_candidates_for`], which is what this concatenates (SH-648).
pub(crate) fn ordered_candidates(
    tx: &impl ReadOps,
) -> Result<Vec<VerificationCandidate>, crate::store::StoreError> {
    let mut candidates = Vec::new();
    for project in tx.projects()? {
        candidates.extend(ordered_candidates_for(tx, project.id)?);
    }
    sort_candidates(&mut candidates);
    Ok(candidates)
}

/// One project's submitted stories, read from an existing transaction, in
/// the order its verifier drains them. The dashboard combines this queue
/// snapshot with its story snapshot in one transaction;
/// [`VerificationQueue::ordered_for`] delegates here so verifier and dashboard
/// ordering cannot drift (SH-549). A project the store does not know yields
/// an empty queue rather than an error: a worker whose project was deleted
/// reads that as "nothing to do" and retires itself.
pub(crate) fn ordered_candidates_for(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<Vec<VerificationCandidate>, crate::store::StoreError> {
    let mut candidates = Vec::new();
    if let Some(project) = tx.project(project)? {
        let checkout = tx.checkout_path(project.id)?;
        let registered =
            super::pr_link::github_repos_from_remotes(&tx.project_remotes(project.id)?);
        let rows = tx.stories(project.id, &StoryQuery::all().state(VERIFYING_STATE))?;
        let stories = super::query::story_map(tx, project.id)?;
        for row in rows {
            if crate::domain::is_blocked(&row.snapshot, &stories) {
                continue;
            }
            let links = tx
                .open_pr_links_for_story(project.id, row.story_no)?
                .into_iter()
                .filter(|link| link.close_on_merge)
                .collect::<Vec<_>>();
            let pull_request = match (&checkout, links.as_slice()) {
                (None, _) => Err(VerificationProblem::MissingCheckout),
                (Some(_), [link])
                    if parse_pr_url(&link.url).is_ok_and(|reference| {
                        registered.iter().any(|repo| {
                            repo.host.eq_ignore_ascii_case(&reference.host)
                                && repo.owner.eq_ignore_ascii_case(&reference.owner)
                                && repo.repo.eq_ignore_ascii_case(&reference.repo)
                        })
                    }) =>
                {
                    Ok(link.clone())
                }
                (Some(_), [link]) => Err(VerificationProblem::UnregisteredPullRequest {
                    url: link.url.clone(),
                    registered: registered
                        .iter()
                        .map(|repo| format!("{}/{}/{}", repo.host, repo.owner, repo.repo))
                        .collect(),
                }),
                (Some(_), []) => Err(VerificationProblem::MissingPullRequest),
                (Some(_), many) => Err(VerificationProblem::MultiplePullRequests(
                    many.iter().map(|link| link.url.clone()).collect(),
                )),
            };
            let entry = verifying_entry(tx, project.id, row.story_no)?;
            let (verifying_since, verifying_generation) = entry
                .map(|(at, generation)| (Some(at), Some(generation)))
                .unwrap_or((None, None));
            candidates.push(VerificationCandidate {
                project: project.id,
                project_slug: project.slug.clone(),
                story_id: row.story_no.to_id(&project.prefix),
                title: row.title,
                priority: row.priority,
                created_at: row.created_at,
                verifying_since,
                verifying_generation,
                blocking_revision: blocking_revision(tx, project.id, row.story_no)?,
                checkout: checkout.clone().unwrap_or_default(),
                cleanup_lease: latest_cleanup_lease(tx, project.id, row.story_no)?,
                pull_request,
            });
        }
    }
    sort_candidates(&mut candidates);
    Ok(candidates)
}

/// The lease paired with the story's latest entry into verification.
///
/// The service writes the lease immediately after `StoryStateChanged`, in the
/// same event batch. Requiring that adjacency means a later legacy/manual
/// unleased submission shadows every older lease by construction rather than
/// accidentally reusing stale resource ownership.
fn latest_cleanup_lease(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<StoryCleanupLease>, AppError> {
    let events = tx.events_for(project, story)?;
    let Some(verifying_index) = events.iter().rposition(|event| {
        matches!(
            event.known(),
            Some(StoryEvent::StoryStateChanged { state, .. }) if state == VERIFYING_STATE
        )
    }) else {
        return Ok(None);
    };
    Ok(events
        .get(verifying_index + 1)
        .and_then(|event| event.known())
        .and_then(|event| match event {
            StoryEvent::StoryCleanupLeaseRecorded { lease, .. } => Some(lease.as_ref().clone()),
            _ => None,
        }))
}

fn sort_candidates(candidates: &mut [VerificationCandidate]) {
    candidates.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            // An unknown wait must not outrank a known wait at equal priority.
            .then_with(|| {
                left.verifying_since
                    .is_none()
                    .cmp(&right.verifying_since.is_none())
            })
            .then_with(|| left.verifying_since.cmp(&right.verifying_since))
            .then_with(|| left.project_slug.cmp(&right.project_slug))
            .then_with(|| left.story_id.cmp(&right.story_id))
    });
}

// Completed stories carry no queue-entry time; retain their cleanup order.
fn sort_cleanup_candidates(candidates: &mut [VerificationCandidate]) {
    candidates.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.project_slug.cmp(&right.project_slug))
            .then_with(|| left.story_id.cmp(&right.story_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_submission_receipts_report_verified_heads_in_central_comments() {
        use crate::domain::SubmissionReceipt;
        use storyhook_test_support::ChildGuard;

        // Run the real helper against isolated Git remotes and endpoint data,
        // then give its unmodified typed PR receipts to the production writer.
        let capture = tempfile::NamedTempFile::new_in("/tmp").unwrap();
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/story/tests/test-submit-head-reporting.sh");
        let mut command = std::process::Command::new("bash");
        command
            .arg(script)
            .env_remove("STORYHOOK_TEST_HOME")
            .env("SH713_RECEIPTS_PATH", capture.path());
        let output = ChildGuard::spawn_with_output(&mut command)
            .unwrap()
            .wait_with_output_within(std::time::Duration::from_secs(120), || {
                "isolated submission-head helper regression".into()
            });
        let captured = std::fs::read_to_string(capture.path()).unwrap();
        let rows: Vec<serde_json::Value> = captured
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 6, "{output:?}\n{captured}");
        let mut mismatches = Vec::new();
        for row in rows {
            let scenario = row["scenario"].as_str().unwrap();
            let expected = row["expected_head"].as_str().unwrap();
            let api_head = row["api_head"].as_str().unwrap();
            let receipt: SubmissionReceipt =
                serde_json::from_value(row["receipt"].clone()).unwrap();
            assert!(receipt.ok);
            let pull_request = receipt.pull_request.unwrap();

            let fixture = storyhook_test_support::ServiceFixture::new();
            fixture.link_origin("https://github.com/acme/widgets");
            let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
            let ctx = Ctx::new(
                &store,
                ProjectId::new(fixture.project().get()),
                fixture.cwd(),
                crate::env::Environment::at(fixture.cwd()),
            )
            .no_hooks(true);
            let service = crate::service::StoryService::new(&ctx);
            let id = service
                .create(&crate::service::NewStoryInput {
                    title: scenario.into(),
                    ..Default::default()
                })
                .unwrap()
                .id;
            service
                .set_state(&id, "verifying", None, None, None)
                .unwrap();
            let queue = VerificationQueue::new(&store);
            let mut candidate = queue.next().unwrap().unwrap();
            // The helper's project has been cleaned up; this service fixture
            // owns the comment transaction, with the same submitted branch.
            let mut lease = receipt.lease.unwrap();
            lease.project_slug.clone_from(&candidate.project_slug);
            lease.story_id.clone_from(&candidate.story_id);
            candidate.cleanup_lease = Some(lease);
            assert!(matches!(
                queue
                    .record_generation_submitted(&ctx, &candidate, &pull_request)
                    .unwrap(),
                GenerationWrite::Applied(_)
            ));
            let story = store
                .read(|tx| tx.story(ctx.project(), StoryNo::parse_id("SH", &id).unwrap()))
                .unwrap()
                .unwrap();
            let comment = story
                .snapshot
                .comments
                .iter()
                .find(|comment| comment.text.starts_with(VERIFICATION_SUBMITTED_PREFIX))
                .unwrap();
            if !comment
                .text
                .contains(&format!("is on origin at {expected};"))
                || (expected != api_head && comment.text.contains(api_head))
            {
                mismatches.push(format!("{scenario}: {}", comment.text));
            }
        }
        assert!(
            mismatches.is_empty(),
            "central comments used stale PR metadata:\n{}",
            mismatches.join("\n")
        );
        assert!(output.status.success(), "{output:?}");
    }

    fn candidate(
        priority: Priority,
        verifying_since: Option<&str>,
        project: &str,
        id: &str,
    ) -> VerificationCandidate {
        VerificationCandidate {
            project: ProjectId::new(1),
            project_slug: project.into(),
            story_id: id.into(),
            title: id.into(),
            priority,
            created_at: "2026-01-01T00:00:00Z".into(),
            verifying_since: verifying_since.map(str::to_string),
            verifying_generation: None,
            blocking_revision: None,
            checkout: PathBuf::new(),
            cleanup_lease: None,
            pull_request: Err(VerificationProblem::MissingPullRequest),
        }
    }

    #[test]
    fn blocked_generation_cannot_record_outcomes_or_comments() {
        let f = storyhook_test_support::ServiceFixture::new();
        let store = crate::store::SqliteStore::open(f.store().path()).unwrap();
        let ctx = Ctx::new(
            &store,
            ProjectId::new(f.project().get()),
            f.cwd(),
            crate::env::Environment::at(f.cwd()),
        )
        .no_hooks(true);
        let id = crate::service::StoryService::new(&ctx)
            .create(&crate::service::NewStoryInput {
                title: "Blocked result race".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        crate::service::StoryService::new(&ctx)
            .set_state(&id, "verifying", None, None, None)
            .unwrap();
        let queue = VerificationQueue::new(&store);
        let c = queue.next().unwrap().unwrap();
        crate::service::StoryService::new(&ctx)
            .set_awaiting(&id, "repair")
            .unwrap();
        assert!(matches!(
            queue
                .upsert_generation_comment(&ctx, &c, "result", "stale")
                .unwrap(),
            GenerationWrite::Superseded
        ));
        assert!(matches!(
            queue.record_generation_returned(&ctx, &c, "stale").unwrap(),
            GenerationWrite::Superseded
        ));
        assert!(matches!(
            queue
                .record_generation_completed(
                    &ctx,
                    &c,
                    None,
                    "CENTRAL VERIFICATION RED — stale",
                    Some("stale cleanup")
                )
                .unwrap(),
            GenerationWrite::Superseded
        ));
        assert!(
            store
                .read(|tx| tx.verification_incident(ctx.project()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn completed_verdict_transaction_is_idempotent_and_rolls_back_invalid_merge() {
        let f = storyhook_test_support::ServiceFixture::new();
        let store = crate::store::SqliteStore::open(f.store().path()).unwrap();
        let ctx = Ctx::new(
            &store,
            ProjectId::new(f.project().get()),
            f.cwd(),
            crate::env::Environment::at(f.cwd()),
        )
        .no_hooks(true);
        let service = crate::service::StoryService::new(&ctx);
        let id = service
            .create(&crate::service::NewStoryInput {
                title: "atomic completion".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        service
            .set_state(&id, "verifying", None, None, None)
            .unwrap();
        let queue = VerificationQueue::new(&store);
        let candidate = queue.next().unwrap().unwrap();
        let snapshot = || {
            store
                .read(|tx| tx.story(ctx.project(), StoryNo::parse_id("SH", &id).unwrap()))
                .unwrap()
                .unwrap()
        };
        let before = snapshot();
        assert!(
            queue
                .record_generation_completed(
                    &ctx,
                    &candidate,
                    Some("https://github.com/acme/widgets/pull/999"),
                    "CENTRAL VERIFICATION GREEN — invalid",
                    Some("retained cleanup")
                )
                .is_err()
        );
        assert_eq!(snapshot().head_seq, before.head_seq);
        assert!(
            store
                .read(|tx| tx.verification_incident(ctx.project()))
                .unwrap()
                .is_none()
        );
        let record = || {
            queue
                .record_generation_completed(
                    &ctx,
                    &candidate,
                    None,
                    "CENTRAL VERIFICATION RED — named failure",
                    Some("retained cleanup"),
                )
                .unwrap()
        };
        let first = record();
        let recorded = snapshot();
        assert_eq!(record(), first);
        assert_eq!(snapshot().head_seq, recorded.head_seq);
        service
            .set_state(&id, "verifying", None, Some("verifying"), None)
            .unwrap();
        assert!(matches!(record(), GenerationWrite::Superseded));
        let next = queue.next().unwrap().unwrap();
        queue
            .record_generation_completed(
                &ctx,
                &next,
                None,
                "CENTRAL VERIFICATION RED — named failure",
                Some("retained cleanup"),
            )
            .unwrap();
        assert_eq!(
            snapshot()
                .snapshot
                .comments
                .iter()
                .filter(|comment| comment.text == "CENTRAL VERIFICATION RED — named failure")
                .count(),
            2
        );
    }

    #[test]
    fn queue_order_handles_missing_times_and_all_identity_ties() {
        let early = Some("2026-01-01T00:01:00Z");
        let late = Some("2026-01-01T00:02:00Z");
        let expected = [
            candidate(Priority::High, None, "z", "Z-9"),
            candidate(Priority::Medium, early, "a", "A-1"),
            candidate(Priority::Medium, early, "a", "A-2"),
            candidate(Priority::Medium, early, "b", "B-1"),
            candidate(Priority::Medium, late, "a", "A-3"),
            candidate(Priority::Medium, None, "a", "A-4"),
            candidate(Priority::Medium, None, "a", "A-5"),
            candidate(Priority::Medium, None, "b", "B-2"),
            candidate(Priority::Low, early, "a", "A-6"),
        ];

        // Check every pair in both directions, including self-equality.
        // This also prevents an unstable input order from deciding ties.
        for left in 0..expected.len() {
            for right in 0..expected.len() {
                let mut actual = vec![expected[left].clone(), expected[right].clone()];
                sort_candidates(&mut actual);
                assert_eq!(
                    actual,
                    [
                        expected[left.min(right)].clone(),
                        expected[left.max(right)].clone()
                    ]
                );
            }
        }
    }

    #[test]
    fn multiple_pr_diagnosis_names_every_ambiguous_link() {
        let diagnosis = VerificationProblem::MultiplePullRequests(vec![
            "https://example.test/pull/1".into(),
            "https://example.test/pull/2".into(),
        ])
        .message();
        assert!(diagnosis.contains("pull/1"), "{diagnosis}");
        assert!(diagnosis.contains("pull/2"), "{diagnosis}");
    }
}
