//! The centralized release-gate queue (SH-521).
//!
//! Queue membership is a story fact, not a second job record: every OPEN story
//! in required state [`VERIFYING_STATE`] is recoverable work after a daemon
//! restart. One daemon worker asks this service for the first candidate, so a
//! crashed attempt is retried from the same durable source of truth.

use std::path::PathBuf;

use crate::domain::pr_url::parse_pr_url;
use crate::domain::{Priority, StoryCleanupLease, StoryEvent, SuperState, VERIFYING_STATE_SLUG};
use crate::error::AppError;
use crate::store::{
    ExpectedSeq, GlobalSeq, PrLink, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryQuery,
    StoryRow, VerificationFailureDisposition, VerificationIncident, WriteOps,
};

use super::story::{append_state_transition, state_transition_events};
use super::{Ctx, append_and_fold, project_prefix, relation, resolve_story};

/// The required OPEN state that hands a published PR to the verifier.
pub const VERIFYING_STATE: &str = VERIFYING_STATE_SLUG;

/// Durable comment prefix proving the centralized release gate landed a PR.
pub const VERIFICATION_GREEN_PREFIX: &str = "CENTRAL VERIFICATION GREEN —";

/// Durable comment prefix proving post-merge resources were reclaimed.
pub const VERIFICATION_CLEANUP_COMPLETE_PREFIX: &str = "CENTRAL VERIFICATION CLEANUP COMPLETE —";

/// Durable comment prefix for verifier infrastructure failures.
pub(crate) const VERIFICATION_INFRASTRUCTURE_PREFIX: &str = "CENTRAL VERIFICATION INFRASTRUCTURE —";

/// Result of a write whose authority belongs to one verification generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationWrite<T> {
    /// The candidate remained current and the write committed.
    Applied(T),
    /// A later state transition superseded the candidate before the write.
    Superseded,
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
    /// Creation timestamp used as the first tie-break.
    pub created_at: String,
    /// When this story most recently entered [`VERIFYING_STATE`], read from
    /// its own `StoryStateChanged` history rather than `updated_at` (SH-524):
    /// the progress checklist rewrites `updated_at` on every publish, which
    /// would make the story's own recency field lie about queue wait time.
    /// `None` for a [`Self::pull_request`] read that predates this field
    /// (`next_cleanup`'s completed-story pass, where wait time is moot) or
    /// for the vanishingly unlikely case no such event survives.
    pub verifying_since: Option<String>,
    /// Exact change-feed position of the latest transition into
    /// [`VERIFYING_STATE`]. Unlike a story id or timestamp, this cannot be
    /// reused by a later submission of the same story, so verifier ownership
    /// and progress journals can reject stale attempts (SH-549).
    pub verifying_generation: Option<GlobalSeq>,
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
/// project `ctx` selects, releasing the queue for another attempt (SH-573).
///
/// One function for both doors — `POST .../verification/ack` and
/// `story verifier ack <incident-id>` (SH-666) — so their contract cannot
/// drift (SH-136): the id must name the incident that is *current*, still
/// halted rather than retrying, and this project's. A reader of a stale
/// comment therefore cannot acknowledge a newer incident by accident, and an
/// acknowledgement retries nothing by itself: the verifier's next tick does.
pub fn acknowledge_verification_incident<S: Store>(
    ctx: &Ctx<'_, S>,
    incident_id: &str,
) -> Result<VerificationIncident, AppError> {
    let current = ctx.store().read(|tx| tx.verification_incident())?;
    let Some(current) = current else {
        return Err(AppError::Validation(
            "no verification incident is active".into(),
        ));
    };
    if !current.halted {
        return Err(AppError::Validation(
            "the verification incident is still retrying".into(),
        ));
    }
    if current.incident_id != incident_id {
        return Err(AppError::Validation(format!(
            "verification incident `{incident_id}` is stale; current incident is `{}`",
            current.incident_id
        )));
    }
    if current.project != ctx.project() {
        return Err(AppError::Validation(format!(
            "verification incident `{incident_id}` belongs to another project"
        )));
    }
    ctx.store().write(|tx| {
        tx.clear_verification_incident(incident_id)?;
        Ok(())
    })?;
    Ok(current)
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

    /// Returns every submitted story across every project, in the exact order
    /// [`Self::next`] would drain them (SH-524): priority, then creation time,
    /// then project/story identity. A queued candidate's position and wait are
    /// computed from this list, never re-derived from a second query that
    /// could race the one [`Self::next`] itself used.
    pub fn ordered(&self) -> Result<Vec<VerificationCandidate>, AppError> {
        Ok(self.store.read(|tx| ordered_candidates(tx))?)
    }

    /// Returns this story's current submitted generation, if it still has one.
    pub(crate) fn current_for(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<Option<VerificationCandidate>, AppError> {
        Ok(self.store.read(|tx| {
            Ok(ordered_candidates(tx)?.into_iter().find(|current| {
                current.project == candidate.project && current.story_id == candidate.story_id
            }))
        })?)
    }

    /// Atomically records a green result only while `candidate` is current.
    pub(crate) fn record_generation_merged(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        pull_request_url: &str,
        green_comment: &str,
    ) -> Result<GenerationWrite<()>, AppError> {
        let project = candidate.project;
        let now = ctx.now();
        Ok(self.store.write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(&*tx, &row, candidate)? {
                return Ok(GenerationWrite::Superseded);
            }
            let linked = tx
                .open_pr_links_for_story(project, story_no)?
                .into_iter()
                .any(|link| link.close_on_merge && link.url == pull_request_url);
            if !linked {
                return Err(AppError::Validation(format!(
                    "story `{}` no longer links submitted pull request `{pull_request_url}`",
                    candidate.story_id
                ))
                .into());
            }
            let ordered_states = tx.states(project)?;
            let done = ordered_states
                .iter()
                .find(|state| state.slug == "done" && state.super_state == SuperState::Closed)
                .cloned()
                .ok_or_else(|| {
                    AppError::Validation(
                        "project has no required CLOSED `done` state; run `story doctor --fix`"
                            .to_string(),
                    )
                })?;
            let states = tx.state_map(project)?;
            clear_candidate_incident(tx, candidate)?;
            append_state_transition(
                tx,
                project,
                story_no,
                &row,
                &prefix,
                &states,
                &done,
                &now,
                vec![
                    StoryEvent::StoryCommentAdded {
                        at: now.clone(),
                        text: green_comment.to_string(),
                    },
                    StoryEvent::StoryPrMerged {
                        at: now.clone(),
                        url: pull_request_url.to_string(),
                    },
                ],
                ctx.provenance(),
            )?;
            Ok(GenerationWrite::Applied(()))
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
        Ok(self.store.write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(&*tx, &row, candidate)? {
                return Ok(GenerationWrite::Superseded);
            }
            let states = tx.state_map(project)?;
            let target = states.get("in-progress").cloned().ok_or_else(|| {
                AppError::Validation(
                    "project has no required OPEN `in-progress` state; run `story doctor --fix`"
                        .to_string(),
                )
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
        Ok(self.store.write(|tx| {
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
        Ok(self.store.write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(&*tx, &row, candidate)? {
                return Ok(GenerationWrite::Superseded);
            }
            let incident_id = format!("{}:{}", candidate.project.get(), generation.get());
            let mut incident = tx
                .verification_incident()?
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
            let body = format!(
                "{VERIFICATION_INFRASTRUCTURE_PREFIX} {state}\n\nAttempt {} of {retry_attempts}. First failure: {}. Latest attempt: {}.\nThe story remains verifying; its code was not classified red.\n\n{}",
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
        Ok(self.store.write(|tx| {
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

    /// Returns a completed story whose post-merge resources still need reap.
    ///
    /// Active verification is intentionally queried separately and first by
    /// the daemon so a transient cleanup fault cannot starve the release gate.
    pub fn next_cleanup(&self) -> Result<Option<VerificationCandidate>, AppError> {
        Ok(self.store.read(|tx| {
            let mut candidates = Vec::new();
            for project in tx.projects()? {
                let checkout = tx.checkout_path(project.id)?.unwrap_or_default();
                let links = tx.pr_links(project.id)?;
                let rows = tx.stories(project.id, &StoryQuery::all().state("done"))?;
                for row in rows {
                    let passed = row
                        .snapshot
                        .comments
                        .iter()
                        .any(|comment| comment.text.starts_with(VERIFICATION_GREEN_PREFIX));
                    let reaped = row.snapshot.comments.iter().any(|comment| {
                        comment
                            .text
                            .starts_with(VERIFICATION_CLEANUP_COMPLETE_PREFIX)
                    });
                    if !passed || reaped {
                        continue;
                    }
                    let pull_request = links
                        .iter()
                        .find(|(story_no, link)| {
                            *story_no == row.story_no
                                && link.close_on_merge
                                && link.status == "merged"
                        })
                        .map(|(_, link)| link.clone())
                        .ok_or(VerificationProblem::MissingPullRequest);
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
                        checkout: checkout.clone(),
                        cleanup_lease: latest_cleanup_lease(tx, project.id, row.story_no)?,
                        pull_request,
                    });
                }
            }
            sort_candidates(&mut candidates);
            Ok(candidates.into_iter().next())
        })?)
    }

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
        self.store.write(|tx| {
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
            let ordered_states = tx.states(project)?;
            let done = ordered_states
                .iter()
                .find(|state| state.slug == "done" && state.super_state == SuperState::Closed)
                .cloned()
                .ok_or_else(|| {
                    AppError::Validation(
                        "project has no required CLOSED `done` state; run `story doctor --fix`"
                            .to_string(),
                    )
                })?;
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

fn candidate_is_current(
    tx: &impl ReadOps,
    row: &StoryRow,
    candidate: &VerificationCandidate,
) -> Result<bool, StoreError> {
    if row.state != VERIFYING_STATE {
        return Ok(false);
    }
    candidate_is_latest_generation(tx, row, candidate)
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
    if let Some(incident) = tx.verification_incident()?.filter(|incident| {
        incident.project == candidate.project
            && candidate.verifying_generation == Some(incident.generation)
    }) {
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
fn verifying_entry(
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
/// transaction. The dashboard combines this queue snapshot with its story
/// snapshot in one transaction; [`VerificationQueue::ordered`] delegates here
/// so verifier and dashboard ordering cannot drift (SH-549).
pub(crate) fn ordered_candidates(
    tx: &impl ReadOps,
) -> Result<Vec<VerificationCandidate>, crate::store::StoreError> {
    let mut candidates = Vec::new();
    for project in tx.projects()? {
        let checkout = tx.checkout_path(project.id)?;
        let registered =
            super::pr_link::github_repos_from_remotes(&tx.project_remotes(project.id)?);
        let rows = tx.stories(project.id, &StoryQuery::all().state(VERIFYING_STATE))?;
        for row in rows {
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
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.project_slug.cmp(&right.project_slug))
            .then_with(|| left.story_id.cmp(&right.story_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
