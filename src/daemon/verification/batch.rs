//! Verification batching, child 2 of SH-822: assemble, submit and gate a
//! batch (SH-831; spec B4, B5, B10, B11 in `docs/spec/verification-batching.md`).
//!
//! When the actuator batches ([`VerificationActuator::batch`]) and the shadow
//! preview (SH-830) found clean partners for the dequeued head, the verifier
//! locks and submits the partners, merges every member onto the preview's
//! base as merge commits, records the batch, pushes the batch branch, opens
//! the batch pull request, and gates that pull request's exact merge tree.
//! A certified batch lands (SH-832, `landing`): every member gets a landing
//! intent bound to the batch, the batch pull request is merged, and every
//! member completes in one transaction and is reaped under its own lock.
//! Any other verdict releases the batch and its members go back to the
//! single-story queue: the head goes on to its own gate in the same tick.
//! The daemon's own verifier does not batch (council decision D10 on
//! SH-832, which replaced decision D1 on SH-831): SH-841 turns it on when a
//! measured trigger fires.
//!
//! A red batch whose judged tree is the batch tip's is bisected (SH-833,
//! spec B7, `bisect` and `culprit`): prefixes of its merge chain are gated
//! until the member that turns a green prefix red is found. That culprit is
//! returned to its own agent, the members before it land together, and the
//! members after it go back to the queue.
//!
//! The whole batch runs under one authority observer over the head and every
//! member still in play: a member that changes (a resubmission, a hold, a
//! reset) or an operator stop cancels it. A batch's failure never fails the
//! tick; it abandons the batch. Only member submissions, a culprit's return,
//! a cleanup failure and an operator stop during a gate write story state.

use super::*;
use crate::domain::gate_verdict::GateVerdict;
use crate::service::batch_assembly::{AssemblyMember, assemble};
use crate::service::batch_preview::{BatchPreview, PreviewOutcome};
use crate::service::workspace_lock::WorkspaceLock;
use crate::store::{
    BatchBisection, BatchExclusion, BatchExclusionReason, BatchGate, BatchId, BatchMember,
    BatchPhase, BatchPullRequest, BisectionOutcome, StoreError, StoryNo, VerificationBatch,
};
use attempt::Attempt;
use end::retire_leftovers;
pub(super) use landing::{Landed, Landing, land, reap, recover};
use locks::MemberLocks;
use serde::Serialize;

mod attempt;
mod bisect;
mod culprit;
mod end;
mod landing;
mod locks;
mod record;
mod shell;
#[cfg(test)]
mod tests;

/// Ended, retired batches kept per project; older ones are pruned when a new
/// batch is recorded.
const RETAINED_BATCHES: usize = 100;

/// Why a batch that was live at worker start is abandoned (B10).
const INTERRUPTED: &str = "the verifier that formed this batch stopped before the batch ended; \
its members keep their generations and are verified again from the queue";

/// Why a bisection that never recorded its end is settled as interrupted
/// (SH-833).
const BISECTION_INTERRUPTED: &str = "the verifier stopped before this bisection ended; no story \
was blamed, and every member is verified again from the queue";

/// The batch operations an actuator may offer (SH-831). An actuator offers
/// them through [`VerificationActuator::batch`].
pub trait BatchActuator {
    /// Pushes a non-head member's leased branch and leaves exactly one open
    /// pull request for it, as [`VerificationActuator::submit`] does for the
    /// head, with that member's own workspace lock inherited.
    fn submit_member(
        &self,
        member: &VerificationCandidate,
        owner: MemberOwner<'_>,
        cancellation: &Cancellation,
    ) -> Result<SubmittedPullRequest, SubmissionFailure>;
    /// Pushes the batch tip to its branch and leaves exactly one open batch
    /// pull request against the base.
    fn publish(
        &self,
        head: &VerificationCandidate,
        publication: &BatchPublication,
        cancellation: &Cancellation,
    ) -> Result<BatchPullRequest, AppError>;
    /// Gates the batch pull request's exact merge tree without landing it
    /// and without any story's repair admission.
    fn gate(
        &self,
        head: &VerificationCandidate,
        pull_request: &PrLink,
        cancellation: &Cancellation,
    ) -> VerificationOutcome;
    /// Closes the batch pull request if it is open and deletes the batch
    /// branch on origin if it is there.
    fn retire(
        &self,
        head: &VerificationCandidate,
        batch: &VerificationBatch,
        comment: &str,
    ) -> Result<BatchRetirement, AppError>;
    /// Whether `base` requires signed commits, on which a batch of unsigned
    /// merge commits could never land (SH-832 D8).
    fn base_policy(
        &self,
        head: &VerificationCandidate,
        base: &str,
        cancellation: &Cancellation,
    ) -> Result<bool, AppError>;
    /// Reaps a landed member as [`VerificationActuator::reap`] reaps a
    /// story, with that member's own workspace lock inherited: the helper
    /// refuses any other story's lock.
    fn reap_member(
        &self,
        member: &VerificationCandidate,
        owner: MemberOwner<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), AppError>;
    /// Deletes landed members' own branches on origin where GitHub reports
    /// each member merged from it at its recorded head (SH-832 D8).
    fn prune_members(
        &self,
        head: &VerificationCandidate,
        members: &[MemberBranch],
    ) -> Result<Vec<MemberPrune>, AppError>;
    /// Whether the merge of `commit` onto `base` already carries a
    /// qualifying gate receipt in the head's checkout, where `verify-pr.sh`
    /// reads receipts (SH-833): a bisection prefix that needs no gate run.
    /// `Err` is an inspection that could not answer.
    fn certified(
        &self,
        head: &VerificationCandidate,
        base: &str,
        commit: &str,
        cancellation: &Cancellation,
    ) -> Result<bool, AppError>;
    /// Pastes a returned member's diagnosis into its agent pane, with that
    /// member's own workspace lock inherited (SH-833): the helper refuses
    /// any other story's lock.
    fn notify_member(
        &self,
        member: &VerificationCandidate,
        message: &str,
        owner: MemberOwner<'_>,
        cancellation: &Cancellation,
    ) -> Result<NotifyDelivery, AppError>;
    /// Re-dispatches a returned member into its own window and worktree
    /// with the resume clause, with its own workspace lock inherited.
    fn redispatch_member(
        &self,
        member: &VerificationCandidate,
        plan: &ResumePlan,
        owner: MemberOwner<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), AppError>;
}

/// A landed member's own branch, for [`BatchActuator::prune_members`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberBranch {
    /// The member's own pull request.
    pub pull_request: String,
    /// Its branch on origin.
    pub branch: String,
    /// The head the batch merged.
    pub head: String,
}

/// What pruning found for one member branch.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct MemberPrune {
    /// The branch.
    pub branch: String,
    /// `deleted`, or why it was kept: `unmerged`, `moved`, `absent`,
    /// `other-head` or `unreadable`.
    pub result: String,
    /// The evidence behind a kept branch, when there is any.
    #[serde(default)]
    pub detail: Option<String>,
}

/// A batch member's own workspace lock, lent to that member's submission.
pub struct MemberOwner<'a>(pub(crate) &'a WorkspaceLock);

/// What a batch pull request is published as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchPublication {
    /// `storyhook/verify-batch/<id>`.
    pub branch: String,
    /// The batch's last merge commit.
    pub tip: String,
    /// The base branch, origin's default.
    pub base: String,
    /// The pull request title.
    pub title: String,
    /// The pull request body, naming every member's pull request.
    pub body: String,
}

/// What retiring a batch found and did on GitHub.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct BatchRetirement {
    /// The open batch pull request was closed.
    pub closed: bool,
    /// A person had already merged the batch pull request.
    pub merged: bool,
    /// The batch branch was deleted on origin.
    pub deleted: bool,
}

/// One batch as the per-dequeue preview record reads it (SH-830 D7).
#[derive(Clone, Debug, Serialize)]
pub(super) struct BatchSummary {
    /// The batch's id, when a record was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    /// Members, head first.
    members: Vec<String>,
    /// How the record ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<BatchPhase>,
    /// The batch gate's verdict, when a gate ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict: Option<GateVerdict>,
    /// The merge tree the batch gate judged.
    #[serde(skip_serializing_if = "Option::is_none")]
    tree: Option<String>,
    /// How long the whole batch took.
    seconds: u64,
    /// Why it ended as it did.
    detail: String,
    /// The bisection of a red batch: its probes and outcome (SH-833).
    #[serde(skip_serializing_if = "Option::is_none")]
    bisection: Option<BatchBisection>,
}

/// How a batch step ended, for the tick.
pub(super) enum BatchEnd {
    /// No batch was attempted; nothing was written.
    NotFormed,
    /// A batch was attempted and ended; the head goes on to its own gate.
    Done(BatchSummary),
    /// The head's attempt ends here, with `result`; `outcome` is what the
    /// preview record reports for it.
    Tick {
        result: TickResult,
        outcome: Box<VerificationOutcome>,
        summary: BatchSummary,
    },
    /// The batch is admitted to land: the tick merges it with the head's
    /// actuator ([`land`]).
    Land(Box<Landing>),
    /// Bisection found the head to be the culprit (SH-833): `outcome` is its
    /// red gate, which the tick takes as the head's own gate outcome, and
    /// `found_by` names the batch in the head's RED comment.
    HeadRed {
        outcome: Box<VerificationOutcome>,
        found_by: String,
        summary: BatchSummary,
    },
}

impl VerificationGuard {
    /// Lists `view`'s members as this attempt's running batch until the
    /// returned value drops, so a reset of a member ends the batch rather
    /// than failing as busy, and status shows the batch (SH-832 B9). Takes
    /// the registry lock alone, and only while the slot is still this
    /// attempt's (the SH-768 identity rule).
    fn enter_batch(
        &self,
        view: status::ActiveBatch,
        cancellation: Cancellation,
    ) -> BatchMembership<'_> {
        let members = view
            .members
            .iter()
            .filter(|member| **member != view.head)
            .cloned()
            .collect();
        self.with_own_slot(|slot| {
            slot.batch = Some(BatchSlot {
                members,
                cancellation,
                view,
            });
        });
        BatchMembership { owner: self }
    }

    fn with_own_slot(&self, change: impl FnOnce(&mut VerificationSlot)) {
        let mut slots = self
            .registry
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots
            .get_mut(&self.active.project)
            .filter(|slot| slot.active == self.active)
        {
            change(slot);
        }
    }
}

/// Clears the slot's batch when the batch ends, on every path.
pub(super) struct BatchMembership<'a> {
    owner: &'a VerificationGuard,
}

impl BatchMembership<'_> {
    /// Takes a member the batch left out off the slot, so a reset of that
    /// story neither waits for the batch nor ends it.
    pub(super) fn leave(&self, story_id: &str) {
        self.owner.with_own_slot(|slot| {
            if let Some(batch) = slot.batch.as_mut() {
                batch.members.remove(story_id);
                batch.view.members.retain(|member| member != story_id);
            }
        });
    }

    /// Shows the batch's record id and phase in status.
    pub(super) fn show(&self, id: &BatchId, phase: BatchPhase) {
        self.owner.with_own_slot(|slot| {
            if let Some(batch) = slot.batch.as_mut() {
                batch.view.id = Some(id.to_string());
                phase.as_str().clone_into(&mut batch.view.phase);
            }
        });
    }

    /// Shows the red batch `id` being bisected, with the members whose
    /// prefix is being gated now (SH-833). `bisecting` is a status phase
    /// only; no record is stored in it.
    pub(super) fn show_bisecting(&self, id: &BatchId, gated: &[String]) {
        self.owner.with_own_slot(|slot| {
            if let Some(batch) = slot.batch.as_mut() {
                batch.view.id = Some(id.to_string());
                batch.view.members = gated.to_vec();
                BISECTING.clone_into(&mut batch.view.phase);
            }
        });
    }
}

impl Drop for BatchMembership<'_> {
    fn drop(&mut self) {
        self.owner.with_own_slot(|slot| slot.batch = None);
    }
}

/// The status phase of a red batch being bisected (SH-833).
const BISECTING: &str = "bisecting";

/// Settles every batch of `project` its verifier did not end (B10): a live
/// batch is abandoned, and a bisection that never recorded its end is
/// marked interrupted (SH-833). Called when the project's verifier worker
/// starts: a batch exists only inside one synchronous tick of that worker,
/// so either was left by a verifier that stopped. Member stories are not
/// touched; they keep their generations and stay queued. Answers the
/// batches settled.
pub fn abandon_interrupted_batches(
    store: &impl Store,
    env: &Environment,
    project: ProjectId,
) -> Result<Vec<BatchId>, AppError> {
    // A read first: a starting worker must never open a write transaction
    // with nothing to write. Every store fault point fires inside every
    // commit, an empty one included, so such a write kills an armed daemon
    // before it accepts its first connection (SH-693), and holds `BEGIN
    // IMMEDIATE` against every client for nothing. The read and the write
    // share one predicate, `needs_finalization`.
    let unsettled = store.read(|tx| {
        Ok(tx
            .verification_batches(project)?
            .iter()
            .any(VerificationBatch::needs_finalization))
    })?;
    if !unsettled {
        return Ok(Vec::new());
    }
    let now = env.now();
    Ok(store.write(|tx| settle_unfinished(tx, project, INTERRUPTED, &now))?)
}

/// Abandons each abandonable batch with `detail` and marks each bisection
/// with no recorded end interrupted: every record `needs_finalization`
/// selects. Answers the batches written.
fn settle_unfinished(
    tx: &mut impl WriteOps,
    project: ProjectId,
    detail: &str,
    now: &str,
) -> Result<Vec<BatchId>, StoreError> {
    let mut settled = Vec::new();
    for batch in tx.verification_batches(project)? {
        if !batch.needs_finalization() {
            continue;
        }
        let mut next = if batch.phase.is_abandonable() {
            let mut next = batch.advance(BatchPhase::Abandoned, now)?;
            next.detail = Some(detail.to_owned());
            next
        } else {
            let mut next = batch.clone();
            next.revision = batch.revision + 1;
            now.clone_into(&mut next.updated_at);
            next
        };
        if let Some(bisection) = next.bisection.as_mut().filter(|b| b.is_unfinished()) {
            bisection.outcome = Some(BisectionOutcome::Interrupted {
                detail: BISECTION_INTERRUPTED.into(),
            });
        }
        if tx.update_verification_batch(&next, batch.revision)? {
            settled.push(next.id);
        }
    }
    Ok(settled)
}

/// Abandons each live batch the verifier can abandon, with `detail`: the
/// backstop when a batch is recorded while another is still live. Leaves
/// bisections alone, because a probe batch is recorded while its own
/// bisection runs. Answers the batches abandoned.
fn abandon_live(
    tx: &mut impl WriteOps,
    project: ProjectId,
    detail: &str,
    now: &str,
) -> Result<Vec<BatchId>, StoreError> {
    let mut abandoned = Vec::new();
    for batch in tx.verification_batches(project)? {
        if !batch.phase.is_abandonable() {
            continue;
        }
        let mut next = batch.advance(BatchPhase::Abandoned, now)?;
        next.detail = Some(detail.to_owned());
        if tx.update_verification_batch(&next, batch.revision)? {
            abandoned.push(next.id);
        }
    }
    Ok(abandoned)
}

/// One member as the batch plans it.
#[derive(Clone)]
struct Planned {
    candidate: VerificationCandidate,
    story: StoryNo,
    generation: GlobalSeq,
    /// The commit the preview merged; the member's pushed head must equal it.
    commit: String,
    /// The member's own pull request, once its submission is recorded.
    pull_request: String,
}

/// The batch the preview selected, checked against the store.
struct Plan {
    base_branch: String,
    base_commit: String,
    repository: std::path::PathBuf,
    members: Vec<Planned>,
    excluded: Vec<BatchExclusion>,
}

/// Runs the batch step for the head the tick is about to gate (SH-831).
///
/// Answers [`BatchEnd::NotFormed`] without side effects unless the preview
/// selected at least one partner and the project's queue is in ordinary
/// operation (no incident retry, no project-fault recovery).
#[allow(clippy::too_many_arguments)]
pub(super) fn run<S: Store>(
    store: &S,
    env: &Environment,
    bus: &ChangeBus,
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    batching: &dyn BatchActuator,
    head: &VerificationCandidate,
    owner: &VerificationGuard,
    preview: Option<&BatchPreview>,
) -> Result<BatchEnd, AppError> {
    let Some(plan) = plan(store, queue, head, owner, preview)? else {
        return Ok(BatchEnd::NotFormed);
    };
    retire_leftovers(store, env, batching, head);
    let started = Instant::now();
    let cancellation = Cancellation::default();
    let tracked = Mutex::new(
        plan.members
            .iter()
            .map(|member| member.candidate.clone())
            .collect::<Vec<_>>(),
    );
    let membership = owner.enter_batch(
        status::ActiveBatch {
            id: None,
            head: head.story_id.clone(),
            members: plan
                .members
                .iter()
                .map(|member| member.candidate.story_id.clone())
                .collect(),
            phase: "selected".into(),
        },
        cancellation.clone(),
    );
    let mut attempt = Attempt {
        store,
        env,
        queue,
        ctx,
        batching,
        head,
        plan,
        cancellation: &cancellation,
        tracked: &tracked,
        membership: Some(membership),
        locks: None,
        record: None,
        dissolved: None,
        failure: None,
        gate: None,
        bisection: None,
    };
    progress_start(env, head, owner);
    let subscription = bus.subscribe();
    let (steps, authority) = observation::observe_during(
        &subscription,
        &owner.cancellation,
        &cancellation,
        &head.project_slug,
        || {
            let members = tracked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            observation::stale_members(store, &members).map(|stale| stale.is_empty())
        },
        || attempt.steps(),
    );
    attempt.membership = None;
    if let Err(error) = steps {
        attempt.failure = Some(format!("the batch step failed: {error}"));
    }
    let stale = match authority {
        Ok(true) => Vec::new(),
        _ => {
            let members = tracked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            observation::stale_members(store, &members)
                .unwrap_or_else(|error| vec![format!("(authority unreadable: {error})")])
        }
    };
    attempt.finish(owner, stale, started)
}

fn plan<S: Store>(
    store: &S,
    queue: &VerificationQueue<'_, S>,
    head: &VerificationCandidate,
    owner: &VerificationGuard,
    preview: Option<&BatchPreview>,
) -> Result<Option<Plan>, AppError> {
    let Some(preview) = preview else {
        return Ok(None);
    };
    let (Some(base_branch), Some(base_commit)) =
        (preview.base_branch.clone(), preview.base_commit.clone())
    else {
        return Ok(None);
    };
    if preview.outcome != PreviewOutcome::Batch
        || preview.members.len() < 2
        || preview.head != head.story_id
        || owner.is_cancelled()
    {
        return Ok(None);
    }
    let (Some(lease), Some(generation), Ok(link)) = (
        head.cleanup_lease.as_ref(),
        head.verifying_generation,
        head.pull_request.as_ref(),
    ) else {
        return Ok(None);
    };
    let (ordinary, prefix) = store.read(|tx| {
        let recovering = tx
            .project_recoveries(head.project)?
            .iter()
            .any(|recovery| recovery.active);
        let incident = tx.verification_incident(head.project)?.is_some();
        // One live batch per project: a batch still landing (its merge
        // requested or uncertain) is resolved from its intents first.
        let landing = tx
            .verification_batches(head.project)?
            .iter()
            .any(|batch| batch.phase == BatchPhase::Landing);
        let prefix = tx
            .project(head.project)?
            .ok_or_else(|| StoreError::NotFound(format!("project {}", head.project)))?
            .prefix;
        Ok((!recovering && !incident && !landing, prefix))
    })?;
    if !ordinary {
        return Ok(None);
    }
    let story = |id: &str| StoryNo::parse_id(&prefix, id).map_err(AppError::from);
    let mut members = vec![Planned {
        candidate: head.clone(),
        story: story(&head.story_id)?,
        generation,
        commit: preview.members[0].commit.clone(),
        pull_request: link.url.clone(),
    }];
    let mut excluded = Vec::new();
    let queued = queue.ordered_for(head.project)?;
    for selected in &preview.members[1..] {
        let current = queued.iter().find(|candidate| {
            candidate.story_id == selected.story_id
                && candidate.blocked_by.is_empty()
                && !candidate.landing_pending
                && candidate.cleanup_lease.is_some()
        });
        match current.and_then(|candidate| Some((candidate, candidate.verifying_generation?))) {
            Some((candidate, generation)) => members.push(Planned {
                candidate: candidate.clone(),
                story: story(&candidate.story_id)?,
                generation,
                commit: selected.commit.clone(),
                pull_request: String::new(),
            }),
            None => excluded.push(BatchExclusion {
                story_id: selected.story_id.clone(),
                reason: BatchExclusionReason::Superseded,
                detail: "it is no longer queued as the preview saw it".into(),
            }),
        }
    }
    if members.len() < 2 {
        return Ok(None);
    }
    Ok(Some(Plan {
        base_branch,
        base_commit,
        repository: lease.repository_path.clone(),
        members,
        excluded,
    }))
}

fn journal(level: &str, head: &VerificationCandidate, message: &str) {
    super::super::activity::emit(
        level,
        "verifier",
        "event",
        &format!("project={} {}", head.project_slug, head.story_id),
        message,
    );
}

/// Starts the head's progress journal for the batch, as a gate does, so
/// status never reads the batch's preparation as missing evidence.
fn progress_start(env: &Environment, head: &VerificationCandidate, owner: &VerificationGuard) {
    let Some(generation) = head.verifying_generation else {
        return;
    };
    let path = journal_path(env, head);
    let record = serde_json::json!({
        "kind": "run",
        "generation": generation.get(),
        "attempt_id": owner.active.attempt_id,
        "at": env.now(),
    });
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, format!("{record}\n")));
    if let Err(error) = written {
        journal(
            "ERROR",
            head,
            &format!(
                "batch progress journal {} not started: {error}",
                path.display()
            ),
        );
    }
}

/// Appends one checklist item to the head's progress journal.
fn progress_item(env: &Environment, head: &VerificationCandidate, path: &str, status: &str) {
    use std::io::Write as _;
    let journal_file = journal_path(env, head);
    let line = serde_json::json!({"kind": "item", "path": path, "status": status, "at": env.now()});
    let written = std::fs::OpenOptions::new()
        .append(true)
        .open(&journal_file)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
    if let Err(error) = written {
        journal(
            "ERROR",
            head,
            &format!(
                "batch progress item `{path}` not written to {}: {error}",
                journal_file.display()
            ),
        );
    }
}
