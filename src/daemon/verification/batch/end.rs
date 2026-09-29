//! How a batch ends: its record's last phase and verdict, its retirement on
//! GitHub, and the few story writes a batch makes (SH-831).

use super::attempt::Attempt;
use super::*;

impl<S: Store> Attempt<'_, S> {
    /// Ends the batch after its observed steps: the record's last phase and
    /// verdict, its retirement, and the few story writes a batch makes.
    pub(super) fn finish(
        mut self,
        owner: &VerificationGuard,
        stale: Vec<String>,
        started: Instant,
    ) -> Result<BatchEnd, AppError> {
        let stopped = owner.is_cancelled();
        let members: Vec<String> = self
            .plan
            .members
            .iter()
            .map(|member| member.candidate.story_id.clone())
            .collect();
        let gate = self.gate.take();
        let Some(record) = self.record.take() else {
            let detail = if stopped {
                "an operator stopped the attempt before the batch was recorded".to_owned()
            } else if !stale.is_empty() {
                format!("{} changed before the batch was recorded", stale.join(", "))
            } else {
                self.dissolved
                    .take()
                    .or_else(|| self.failure.take())
                    .unwrap_or_else(|| "the batch ended before it was recorded".into())
            };
            journal(
                "INFO",
                self.head,
                &format!("verification batch not formed: {detail}"),
            );
            let summary = BatchSummary {
                id: None,
                members,
                phase: None,
                verdict: None,
                tree: None,
                seconds: started.elapsed().as_secs(),
                detail,
            };
            if stopped {
                return Ok(BatchEnd::Tick {
                    result: stopped_result(self.queue, self.head)?,
                    outcome: Box::new(VerificationOutcome::Cancelled),
                    summary,
                });
            }
            return Ok(BatchEnd::Done(summary));
        };
        let gated = gate.is_some();
        let verdict =
            |outcome: &VerificationOutcome| GateVerdict::of(&Ok(Some(outcome.clone())), false);
        let (phase, detail, batch_gate) = if stopped {
            (
                BatchPhase::Abandoned,
                "an operator stopped the attempt".to_owned(),
                gate.as_ref().map(|(_, seconds)| BatchGate {
                    verdict: GateVerdict::Interrupted,
                    tree: None,
                    detail: "the gate was stopped".into(),
                    seconds: *seconds,
                }),
            )
        } else if !stale.is_empty()
            && !matches!(gate, Some((VerificationOutcome::CleanupFailed { .. }, _)))
        {
            (
                BatchPhase::Abandoned,
                format!(
                    "{} changed during the batch, so its verdict cannot count",
                    stale.join(", ")
                ),
                gate.as_ref().map(|(_, seconds)| BatchGate {
                    verdict: GateVerdict::Withdrawn,
                    tree: None,
                    detail: "a member lost its authority during the gate".into(),
                    seconds: *seconds,
                }),
            )
        } else if let Some((outcome, seconds)) = &gate {
            (
                BatchPhase::Released,
                format!(
                    "gated ({}); landing a batch is not built yet, so every member returns to the single-story queue",
                    verdict(outcome).as_str()
                ),
                Some(BatchGate {
                    verdict: verdict(outcome),
                    tree: judged_tree(outcome),
                    detail: outcome_detail(outcome),
                    seconds: *seconds,
                }),
            )
        } else {
            (
                BatchPhase::Abandoned,
                self.failure
                    .take()
                    .unwrap_or_else(|| "the batch ended before its gate".into()),
                None,
            )
        };
        let mut ended = record.advance(phase, &self.env.now())?;
        ended.detail = Some(detail.clone());
        ended.gate = batch_gate;
        let ended = match write(self.store, &ended, record.revision) {
            Ok(()) => ended,
            Err(error) => {
                journal(
                    "ERROR",
                    self.head,
                    &format!(
                        "verification batch {} could not record its end ({detail}): {error}",
                        record.id
                    ),
                );
                record
            }
        };
        journal(
            "INFO",
            self.head,
            &format!(
                "verification batch {} {}: {detail}",
                ended.id,
                ended.phase.as_str()
            ),
        );
        if !stopped && !ended.phase.is_live() {
            retire(self.store, self.env, self.batching, self.head, &ended);
        }
        let summary = BatchSummary {
            id: Some(ended.id.to_string()),
            members,
            phase: Some(ended.phase),
            verdict: ended.gate.as_ref().map(|gate| gate.verdict),
            tree: ended.gate.as_ref().and_then(|gate| gate.tree.clone()),
            seconds: started.elapsed().as_secs(),
            detail,
        };
        if let Some((outcome @ VerificationOutcome::CleanupFailed { cleanup, .. }, _)) = &gate {
            // The gate's worktree is not safe to reuse, whatever the verdict
            // and whichever member changed: halt as a single gate would.
            if !observation::human_permits(self.store, self.head)? {
                observation::withdraw_with_cleanup_evidence(self.store, self.head, cleanup)?;
                return Ok(BatchEnd::Tick {
                    result: TickResult::Returned,
                    outcome: Box::new(outcome.clone()),
                    summary,
                });
            }
            let detail = format!(
                "the gate of verification batch {} could not clean up: {}: {}\nOwner retained: {}\nWorktree retained: {}\nQuiescence/recovery was not established; do not clear ownership or remove evidence without recovery checks.",
                ended.id,
                cleanup.phase,
                cleanup.detail,
                cleanup
                    .owner
                    .as_deref()
                    .unwrap_or("path not reported by wrapper"),
                cleanup
                    .worktree
                    .as_deref()
                    .unwrap_or("path not reported by wrapper"),
            );
            return Ok(
                match record_infrastructure_failure(
                    self.queue,
                    self.ctx,
                    self.head,
                    VerificationFailureDisposition::Permanent,
                    &detail,
                )? {
                    GenerationWrite::Applied(result) => BatchEnd::Tick {
                        result,
                        outcome: Box::new(outcome.clone()),
                        summary,
                    },
                    GenerationWrite::Superseded => BatchEnd::Done(summary),
                },
            );
        }
        if stopped {
            if gated {
                record_generation_interrupted(
                    self.queue,
                    self.ctx,
                    self.env,
                    self.head,
                    &owner.active,
                )?;
            }
            return Ok(BatchEnd::Tick {
                result: stopped_result(self.queue, self.head)?,
                outcome: Box::new(VerificationOutcome::Cancelled),
                summary,
            });
        }
        Ok(BatchEnd::Done(summary))
    }
}

fn stopped_result<S: Store>(
    queue: &VerificationQueue<'_, S>,
    head: &VerificationCandidate,
) -> Result<TickResult, AppError> {
    Ok(if queue.human_permits(head)? {
        TickResult::Stopped
    } else {
        TickResult::Returned
    })
}

pub(super) fn write(
    store: &impl Store,
    batch: &VerificationBatch,
    expected: i64,
) -> Result<(), AppError> {
    if store.write(|tx| tx.update_verification_batch(batch, expected))? {
        Ok(())
    } else {
        Err(AppError::Storage(format!(
            "verification batch {} changed under its verifier",
            batch.id
        )))
    }
}

/// Retires ended batches of the head's project that are not retired yet: a
/// failed retirement, or a batch abandoned at worker start. Best effort.
pub(super) fn retire_leftovers(
    store: &impl Store,
    env: &Environment,
    batching: &dyn BatchActuator,
    head: &VerificationCandidate,
) {
    match store.read(|tx| tx.verification_batches(head.project)) {
        Ok(batches) => {
            for batch in batches
                .iter()
                .filter(|batch| !batch.phase.is_live() && !batch.retired)
            {
                retire(store, env, batching, head, batch);
            }
        }
        Err(error) => journal(
            "ERROR",
            head,
            &format!("verification batches could not be read for retirement: {error}"),
        ),
    }
}

/// Closes an ended batch's pull request and deletes its branch, then records
/// that it is retired. A failure is journaled and retried at the next batch.
fn retire(
    store: &impl Store,
    env: &Environment,
    batching: &dyn BatchActuator,
    head: &VerificationCandidate,
    batch: &VerificationBatch,
) {
    let comment = format!(
        "Verification batch {} ended ({}). Landing a batch is not built yet, so each member is verified on its own.",
        batch.id,
        batch.gate.as_ref().map_or_else(
            || batch.phase.as_str().to_owned(),
            |gate| format!("{}, {}", batch.phase.as_str(), gate.verdict.as_str())
        )
    );
    match batching.retire(head, batch, &comment) {
        Ok(retirement) => {
            let mut next = batch.clone();
            next.retired = true;
            next.revision = batch.revision + 1;
            next.updated_at = env.now();
            if retirement.merged {
                next.detail = Some(format!(
                    "{} A person merged the batch pull request outside the verifier.",
                    next.detail.unwrap_or_default()
                ));
            }
            if let Err(error) = write(store, &next, batch.revision) {
                journal(
                    "ERROR",
                    head,
                    &format!(
                        "verification batch {} retirement not recorded: {error}",
                        batch.id
                    ),
                );
            }
        }
        Err(error) => journal(
            "ERROR",
            head,
            &format!(
                "verification batch {} could not be retired and is retried at the next batch: {error}",
                batch.id
            ),
        ),
    }
}

pub(super) fn member_list(batch: &VerificationBatch) -> String {
    batch
        .members
        .iter()
        .map(|member| member.story_id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn judged_tree(outcome: &VerificationOutcome) -> Option<String> {
    match outcome {
        VerificationOutcome::Certified { tree, .. }
        | VerificationOutcome::TestsFailed { tree, .. } => Some(tree.clone()),
        VerificationOutcome::CleanupFailed { verdict, .. } => match verdict {
            CompletedVerification::TestsFailed { tree, .. }
            | CompletedVerification::GatePassed { tree, .. }
            | CompletedVerification::Certified { tree, .. } => Some(tree.clone()),
        },
        _ => None,
    }
}

fn outcome_detail(outcome: &VerificationOutcome) -> String {
    match outcome {
        VerificationOutcome::Certified { detail, gate, .. } => format!("`{gate}` passed. {detail}"),
        VerificationOutcome::TestsFailed {
            detail, log, gate, ..
        } => format!("`{gate}` failed. Full log: {log}. {detail}"),
        VerificationOutcome::Conflict { detail }
        | VerificationOutcome::InvalidSubmission { detail }
        | VerificationOutcome::InfrastructureFailure { detail, .. } => detail.clone(),
        VerificationOutcome::ProjectFault { fault } => format!("project fault: {fault:?}"),
        VerificationOutcome::Cancelled => "the gate was cancelled".into(),
        VerificationOutcome::RepairDeferred { .. } => {
            "a repair admission was refused, which a batch gate never claims".into()
        }
        VerificationOutcome::CleanupFailed { cleanup, .. } => {
            format!("cleanup failed: {}: {}", cleanup.phase, cleanup.detail)
        }
    }
}
