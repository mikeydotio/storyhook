//! Landing a certified batch (SH-832; spec B5, B6, B10): admission after the
//! batch observer, the merge through the head's own actuator, completion of
//! every member in one transaction, a reap of each member under its own
//! workspace lock, and recovery of an interrupted landing from its intents.
//!
//! Admission runs after the observer on purpose: once a member holds a
//! landing intent it is `landing_pending`, which the observer would read as
//! a change. The admitting transaction re-derives every member instead.

use super::attempt::Attempt;
use super::end::{outcome_detail, retire};
use super::*;
use crate::service::batch_landing::BatchLandingAdmission;
use crate::service::landing::VerifiedSubmission;
use crate::store::{BatchLandingIntent, LandingIntent};

/// A certified batch admitted to land, handed from the batch step to the
/// tick, which merges it with the head's own actuator ([`land`]).
pub(in crate::daemon::verification) struct Landing {
    intent: BatchLandingIntent,
    batch: BatchId,
    members: Vec<VerificationCandidate>,
    locks: Option<MemberLocks>,
    /// The batch gate's evidence, quoted in every member's GREEN.
    detail: String,
    /// The batch gate's outcome, for the preview record.
    pub(in crate::daemon::verification) outcome: VerificationOutcome,
    /// The batch as the preview record reads it.
    pub(in crate::daemon::verification) summary: BatchSummary,
}

/// How a landing left the tick.
pub(in crate::daemon::verification) enum Landed {
    /// Every completable member is done; release locks and retire batch artifacts
    /// after the in-flight record is released.
    Reap(Box<Reaps>),
    /// The head's attempt ends with this result.
    Tick(TickResult),
    /// The merge was never requested: the batch is released and the head
    /// goes on to its own gate in this tick (SH-831 D2).
    Released,
}

/// Batch artifacts and member locks retained until completion commits.
pub(in crate::daemon::verification) struct Reaps {
    batch: BatchId,
    locks: Option<MemberLocks>,
}

impl<S: Store> Attempt<'_, S> {
    /// Admits the certified batch to land in one transaction: a landing
    /// intent per member and the record moved to `landing`. `Err` is why it
    /// may not land; nothing was written then.
    pub(super) fn admit(
        &mut self,
        record: &VerificationBatch,
        outcome: &VerificationOutcome,
        seconds: u64,
        summary: BatchSummary,
    ) -> Result<Landing, String> {
        let VerificationOutcome::Certified {
            head,
            tree,
            detail,
            gate,
        } = outcome
        else {
            return Err("only a certified batch lands".into());
        };
        let certification = VerifiedSubmission {
            head: head.clone(),
            tree: tree.clone(),
            gate: gate.clone(),
        };
        // The record's own members: a batch lands exactly what it gated.
        let members: Vec<VerificationCandidate> = record
            .members
            .iter()
            .filter_map(|member| {
                self.plan
                    .members
                    .iter()
                    .find(|planned| planned.candidate.story_id == member.story_id)
                    .map(|planned| planned.candidate.clone())
            })
            .collect();
        let gated = BatchGate {
            verdict: GateVerdict::Certified,
            tree: Some(tree.clone()),
            detail: outcome_detail(outcome),
            seconds,
        };
        match self
            .queue
            .begin_batch_landing(self.ctx, record, &members, &certification, gated)
        {
            Ok(BatchLandingAdmission::Admitted { intent, record }) => {
                journal(
                    "INFO",
                    self.head,
                    &format!(
                        "verification batch {} landing: {} at {}",
                        record.id,
                        end::member_list(&record),
                        record.tip
                    ),
                );
                Ok(Landing {
                    intent: *intent,
                    batch: record.id.clone(),
                    members,
                    locks: self.locks.take(),
                    detail: detail.clone(),
                    outcome: outcome.clone(),
                    summary,
                })
            }
            Ok(BatchLandingAdmission::Refused(why)) => Err(why),
            Err(error) => Err(format!("admitting its landing failed: {error}")),
        }
    }
}

/// Merges an admitted batch through the head's actuator and records the
/// outcome: every completable member done in one transaction, the landing
/// released when its merge was never requested, or every member left
/// fenced when the outcome is uncertain.
#[allow(clippy::too_many_arguments)]
pub(in crate::daemon::verification) fn land<S: Store, A: VerificationActuator>(
    store: &S,
    env: &Environment,
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    actuator: &A,
    head: &VerificationCandidate,
    owner: &VerificationGuard,
    landing: &mut Landing,
) -> Result<Landed, AppError> {
    // Status shows the batch while it lands (B9); dropped on every path.
    let _membership = owner.enter_batch(
        status::ActiveBatch {
            id: Some(landing.batch.to_string()),
            head: head.story_id.clone(),
            members: landing
                .members
                .iter()
                .map(|member| member.story_id.clone())
                .collect(),
            phase: BatchPhase::Landing.as_str().into(),
        },
        Cancellation::default(),
    );
    let row = landing.intent.row(&head.story_id).cloned().ok_or_else(|| {
        AppError::Storage(format!(
            "verification batch {} has no landing intent for its head {}",
            landing.batch, head.story_id
        ))
    })?;
    let outcome = actuator.land(head, &row);
    journal(
        if matches!(outcome, LandingOutcome::Merged { .. }) {
            "INFO"
        } else {
            "ERROR"
        },
        head,
        &format!("verification batch {} landing: {outcome:?}", landing.batch),
    );
    if !observation::human_permits(store, head)? {
        return Ok(Landed::Tick(TickResult::Returned));
    }
    match outcome {
        LandingOutcome::Merged { detail } => {
            let landed = complete(
                env,
                queue,
                ctx,
                head,
                owner,
                &landing.intent,
                &format!("{} {detail}", landing.detail),
                &landing.members,
                landing.locks.take(),
            )?;
            if matches!(landed, Landed::Reap(_)) {
                landing.summary.phase = Some(BatchPhase::Landed);
                landing.summary.detail = "landed; every member done".into();
            }
            Ok(landed)
        }
        LandingOutcome::NotAttempted { detail } => {
            let released = queue.release_unattempted_batch_landing(
                ctx,
                &landing.intent,
                &format!(
                    "gated (certified); the merge was never requested, so every member returns to the single-story queue: {detail}"
                ),
            )?;
            journal(
                "INFO",
                head,
                &format!(
                    "verification batch {} released: its merge was never requested",
                    released.id
                ),
            );
            if let Some(batching) = actuator.batch() {
                retire(store, env, batching, head, &released);
            }
            landing.summary.phase = Some(BatchPhase::Released);
            landing
                .summary
                .detail
                .clone_from(&released.detail.unwrap_or_default());
            Ok(Landed::Released)
        }
        LandingOutcome::Uncertain { detail } => {
            for member in &landing.members {
                StoryService::new(ctx).comment(
                    &member.story_id,
                    &format!(
                        "CENTRAL LANDING PENDING — verification batch {} landing {} remains fenced; every member stays in verifying until the batch merge is confirmed.\n\n{}",
                        landing.batch,
                        landing.intent.batch.landing,
                        crate::text_lint::quote_evidence(&detail)
                    ),
                )?;
            }
            Ok(Landed::Tick(TickResult::RetryLater))
        }
    }
}

/// Recovers an interrupted batch landing whose merge `recover_landing`
/// confirmed through `candidate`'s intent (B10): completes every member now
/// queued and permitted, and reaps them. Nothing completable is
/// [`TickResult::RetryLater`], never `Returned`, so a member a person holds
/// cannot make the worker spin.
#[allow(clippy::too_many_arguments)]
pub(in crate::daemon::verification) fn recover<S: Store, A: VerificationActuator>(
    store: &S,
    env: &Environment,
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    actuator: &A,
    candidate: &VerificationCandidate,
    owner: &VerificationGuard,
    intent: &LandingIntent,
    detail: &str,
    ordered: &[VerificationCandidate],
) -> Result<TickResult, AppError> {
    let Some(binding) = &intent.batch else {
        return Err(AppError::Storage(format!(
            "landing {} is not a batch member's",
            intent.id
        )));
    };
    let Some(pending) = BatchLandingIntent::collect(&store.read(|tx| tx.landing_intents())?)
        .into_iter()
        .find(|pending| pending.batch.id == binding.id)
    else {
        return Ok(TickResult::RetryLater);
    };
    let members: Vec<VerificationCandidate> = pending
        .rows
        .iter()
        .filter_map(|row| {
            ordered
                .iter()
                .find(|queued| queued.story_id == row.story_id)
                .cloned()
        })
        .collect();
    let others: Vec<(StoryNo, String)> = pending
        .rows
        .iter()
        .filter(|row| {
            row.story_id != candidate.story_id
                && members.iter().any(|member| member.story_id == row.story_id)
        })
        .map(|row| (row.story, row.story_id.clone()))
        .collect();
    let locks = match actuator.batch() {
        Some(_) => Some(MemberLocks::acquire(&candidate.checkout, &others)?.0),
        None => None,
    };
    match complete(
        env, queue, ctx, candidate, owner, &pending, detail, &members, locks,
    )? {
        Landed::Reap(reaps) => reap(store, env, ctx, actuator, candidate, owner, *reaps),
        Landed::Tick(result) => Ok(result),
        Landed::Released => Ok(TickResult::RetryLater),
    }
}

/// Completes every member the verifier may complete, in one transaction.
#[allow(clippy::too_many_arguments)]
fn complete<S: Store>(
    env: &Environment,
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    head: &VerificationCandidate,
    owner: &VerificationGuard,
    intent: &BatchLandingIntent,
    detail: &str,
    members: &[VerificationCandidate],
    locks: Option<MemberLocks>,
) -> Result<Landed, AppError> {
    // Declared before the write that takes the members out of the queue, so
    // status never sees them gone without the reason (SH-768).
    let pending = owner.reserve(ReservationReason::Cleanup, env.now());
    let completed = queue.complete_batch_landing(ctx, intent, detail, members)?;
    journal(
        "INFO",
        head,
        &format!(
            "verification batch {} landed: {} done",
            intent.batch.id,
            if completed.is_empty() {
                "no member".to_owned()
            } else {
                completed.join(", ")
            }
        ),
    );
    if !completed.contains(&head.story_id) {
        // A person holds this story (or nothing was completable); its row
        // stays for reconciliation, as a single human-only landing's does.
        return Ok(Landed::Tick(if completed.is_empty() {
            TickResult::RetryLater
        } else {
            TickResult::Returned
        }));
    }
    pending.retire();
    Ok(Landed::Reap(Box::new(Reaps {
        batch: intent.batch.id.clone(),
        locks,
    })))
}

/// Releases member locks to the closure worker and retires remote batch artifacts.
pub(in crate::daemon::verification) fn reap<S: Store, A: VerificationActuator>(
    store: &S,
    env: &Environment,
    _ctx: &Ctx<'_, S>,
    actuator: &A,
    head: &VerificationCandidate,
    _owner: &VerificationGuard,
    reaps: Reaps,
) -> Result<TickResult, AppError> {
    let batching = actuator.batch();
    // Each member's committed closure schedules its own cleanup. Releasing
    // these locks hands ownership to that controller; no member is reaped here.
    drop(reaps.locks);
    if let Some(batching) = batching {
        match store.read(|tx| tx.verification_batches(head.project)) {
            Ok(batches) => {
                if let Some(landed) = batches
                    .iter()
                    .find(|batch| batch.id == reaps.batch && !batch.retired)
                {
                    retire(store, env, batching, head, landed);
                }
            }
            Err(error) => journal(
                "ERROR",
                head,
                &format!(
                    "verification batch {} could not be read for retirement: {error}",
                    reaps.batch
                ),
            ),
        }
    }
    Ok(TickResult::Completed)
}
