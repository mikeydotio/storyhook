//! Verification batching, child 2 of SH-822: assemble, submit and gate a
//! batch (SH-831; spec B4, B5, B10, B11 in `docs/spec/verification-batching.md`).
//!
//! When the actuator batches ([`VerificationActuator::batch`]) and the shadow
//! preview (SH-830) found clean partners for the dequeued head, the verifier
//! locks and submits the partners, merges every member onto the preview's
//! base as merge commits, records the batch, pushes the batch branch, opens
//! the batch pull request, and gates that pull request's exact merge tree.
//! Landing a batch is not built yet (SH-832), so whatever the verdict the
//! batch is recorded and its members go back to the single-story queue: the
//! head goes on to its own gate in the same tick. The daemon's own verifier
//! does not batch until landing exists (decision D1 on SH-831).
//!
//! The whole batch runs under one authority observer over the head and every
//! member: a member that changes (a resubmission, a hold, a reset) or an
//! operator stop cancels it. A batch's failure never fails the tick; it
//! abandons the batch. Only member submissions, a cleanup failure and an
//! operator stop during the gate write story state.

use super::*;
use crate::domain::gate_verdict::GateVerdict;
use crate::service::batch_assembly::{AssemblyMember, assemble};
use crate::service::batch_preview::{BatchPreview, PreviewOutcome};
use crate::service::workspace_lock::WorkspaceLock;
use crate::store::{
    BatchExclusion, BatchExclusionReason, BatchGate, BatchId, BatchMember, BatchPhase,
    BatchPullRequest, StoreError, StoryNo, VerificationBatch,
};
use attempt::Attempt;
use end::retire_leftovers;
use locks::MemberLocks;
use serde::Serialize;

mod attempt;
mod end;
mod locks;
mod shell;
#[cfg(test)]
mod tests;

/// Ended, retired batches kept per project; older ones are pruned when a new
/// batch is recorded.
const RETAINED_BATCHES: usize = 100;

/// Why a batch that was live at worker start is abandoned (B10).
const INTERRUPTED: &str = "the verifier that formed this batch stopped before the batch ended; \
its members keep their generations and are verified again from the queue";

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
}

impl VerificationGuard {
    /// Lists `members` as this attempt's running batch until the returned
    /// value drops, so a reset of a member ends the batch rather than failing
    /// as busy. Takes the registry lock alone, and only while the slot is
    /// still this attempt's (the SH-768 identity rule).
    fn enter_batch(
        &self,
        members: BTreeSet<String>,
        cancellation: Cancellation,
    ) -> BatchMembership<'_> {
        self.with_own_slot(|slot| {
            slot.batch = Some(BatchSlot {
                members,
                cancellation,
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
            }
        });
    }
}

impl Drop for BatchMembership<'_> {
    fn drop(&mut self) {
        self.owner.with_own_slot(|slot| slot.batch = None);
    }
}

/// Abandons every batch of `project` that is still live (B10). Called when
/// the project's verifier worker starts: a batch exists only inside one
/// synchronous tick of that worker, so a live one was left by a verifier
/// that stopped. Member stories are not touched; they keep their
/// generations and stay queued. Answers the batches abandoned.
pub fn abandon_interrupted_batches(
    store: &impl Store,
    env: &Environment,
    project: ProjectId,
) -> Result<Vec<BatchId>, AppError> {
    // A read first: a starting worker must never open a write transaction
    // with nothing to write. Every store fault point fires inside every
    // commit, an empty one included, so such a write kills an armed daemon
    // before it accepts its first connection (SH-693), and holds `BEGIN
    // IMMEDIATE` against every client for nothing.
    let live = store.read(|tx| {
        Ok(tx
            .verification_batches(project)?
            .iter()
            .any(|batch| batch.phase.is_abandonable()))
    })?;
    if !live {
        return Ok(Vec::new());
    }
    let now = env.now();
    Ok(store.write(|tx| abandon_live(tx, project, INTERRUPTED, &now))?)
}

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
        plan.members
            .iter()
            .skip(1)
            .map(|member| member.candidate.story_id.clone())
            .collect(),
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
        record: None,
        dissolved: None,
        failure: None,
        gate: None,
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
        let prefix = tx
            .project(head.project)?
            .ok_or_else(|| StoreError::NotFound(format!("project {}", head.project)))?
            .prefix;
        Ok((!recovering && !incident, prefix))
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
