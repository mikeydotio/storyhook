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
        if self.bisection.is_some() {
            return self.finish_bisection(owner, stale, started);
        }
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
                bisection: None,
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
        // A certified batch whose members all kept their authority lands
        // (SH-832). A refused admission wrote nothing: the batch is released
        // below with the reason, and the head goes on to its own gate.
        let mut refused = None;
        if !stopped
            && stale.is_empty()
            && let Some((outcome @ VerificationOutcome::Certified { tree, .. }, seconds)) = &gate
        {
            let summary = BatchSummary {
                id: Some(record.id.to_string()),
                members: members.clone(),
                phase: Some(BatchPhase::Landing),
                verdict: Some(GateVerdict::Certified),
                tree: Some(tree.clone()),
                seconds: started.elapsed().as_secs(),
                detail: "certified; landing".into(),
                bisection: None,
            };
            match self.admit(&record, outcome, *seconds, summary) {
                Ok(landing) => return Ok(BatchEnd::Land(Box::new(landing))),
                Err(why) => refused = Some(why),
            }
        }
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
                match &refused {
                    Some(why) => format!(
                        "gated (certified), but its landing was refused, so every member returns to the single-story queue: {why}"
                    ),
                    None => format!(
                        "gated ({}); only a certified batch lands, so every member returns to the single-story queue",
                        verdict(outcome).as_str()
                    ),
                },
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
            bisection: None,
        };
        if let Some((outcome @ VerificationOutcome::CleanupFailed { .. }, _)) = &gate {
            return self.halt(&ended.id, outcome, summary);
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

impl<S: Store> Attempt<'_, S> {
    /// Halts the queue after a batch gate (or a bisection probe of it) that
    /// could not clean up: the gate's worktree is not safe to reuse, whatever
    /// the verdict and whichever member changed, so it halts as a single
    /// gate would.
    pub(super) fn halt(
        &self,
        batch: &BatchId,
        outcome: &VerificationOutcome,
        summary: BatchSummary,
    ) -> Result<BatchEnd, AppError> {
        let VerificationOutcome::CleanupFailed { cleanup, .. } = outcome else {
            return Ok(BatchEnd::Done(summary));
        };
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
            batch,
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
        Ok(
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
        )
    }
}

pub(super) fn stopped_result<S: Store>(
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
/// that it is retired; for a landed batch, deletes its members' own branches
/// on origin instead (SH-832 D8). A failure is journaled and retried at the
/// next batch.
pub(super) fn retire(
    store: &impl Store,
    env: &Environment,
    batching: &dyn BatchActuator,
    head: &VerificationCandidate,
    batch: &VerificationBatch,
) {
    if batch.phase == BatchPhase::Landed {
        retire_landed(store, env, batching, head, batch);
        return;
    }
    let comment = retirement_comment(batch);
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

/// Retires a landed batch: land-pr.sh already deleted the batch branch, so
/// what is left is each member's own branch on origin, deleted only where
/// GitHub reports that member merged from it at its head. The branches kept
/// are named in the record's detail; the call is not repeated once it
/// answered.
fn retire_landed(
    store: &impl Store,
    env: &Environment,
    batching: &dyn BatchActuator,
    head: &VerificationCandidate,
    batch: &VerificationBatch,
) {
    // A member a person holds still has its landing intent and is not done:
    // its branch is its own until it completes.
    let held = match store.read(|tx| tx.landing_intents()) {
        Ok(intents) => intents
            .into_iter()
            .filter(|intent| {
                intent
                    .batch
                    .as_ref()
                    .is_some_and(|binding| binding.id == batch.id)
            })
            .map(|intent| intent.story)
            .collect::<Vec<_>>(),
        Err(error) => {
            journal(
                "ERROR",
                head,
                &format!(
                    "verification batch {} landing intents could not be read for retirement: {error}",
                    batch.id
                ),
            );
            return;
        }
    };
    let branches: Vec<MemberBranch> = batch
        .members
        .iter()
        .filter(|member| !held.contains(&member.story))
        .filter_map(|member| {
            Some(MemberBranch {
                pull_request: member.pull_request.clone(),
                branch: member.branch.clone()?,
                head: member.head_commit.clone(),
            })
        })
        .collect();
    let kept = if branches.is_empty() {
        Vec::new()
    } else {
        match batching.prune_members(head, &branches) {
            Ok(pruned) => pruned
                .into_iter()
                .filter(|member| member.result != "deleted" && member.result != "absent")
                .map(|member| {
                    format!(
                        "{} ({}{})",
                        member.branch,
                        member.result,
                        member
                            .detail
                            .map(|detail| format!(": {detail}"))
                            .unwrap_or_default()
                    )
                })
                .collect(),
            Err(error) => {
                journal(
                    "ERROR",
                    head,
                    &format!(
                        "verification batch {} member branches could not be pruned and are retried at the next batch: {error}",
                        batch.id
                    ),
                );
                return;
            }
        }
    };
    let mut next = batch.clone();
    next.retired = true;
    next.revision = batch.revision + 1;
    next.updated_at = env.now();
    if !kept.is_empty() {
        next.detail = Some(format!(
            "{} Member branches kept on origin: {}.",
            next.detail.unwrap_or_default(),
            kept.join(", ")
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

/// What a batch's pull request is closed with: why it ended without
/// landing, and for a bisection (SH-833) what the bisection found.
fn retirement_comment(batch: &VerificationBatch) -> String {
    let ended = batch.gate.as_ref().map_or_else(
        || batch.phase.as_str().to_owned(),
        |gate| format!("{}, {}", batch.phase.as_str(), gate.verdict.as_str()),
    );
    if let Some(of) = &batch.bisects {
        return format!(
            "Verification batch {} was a bisection probe of batch {} (its first {} members). It ended ({ended}) without landing; its verdict is recorded on batch {}.",
            batch.id, of.parent, of.prefix, of.parent
        );
    }
    let found = match batch.bisection.as_ref().and_then(|b| b.outcome.as_ref()) {
        Some(BisectionOutcome::Culprit {
            story_id,
            certified,
            ..
        }) => format!(
            " Bisection found that {story_id} turns it red; {} leading members were certified.",
            certified
        ),
        Some(
            BisectionOutcome::Inconclusive { detail } | BisectionOutcome::Interrupted { detail },
        ) => format!(" Bisection blamed no member: {detail}."),
        None => {
            return format!(
                "Verification batch {} ended ({ended}) without landing, so each member is verified on its own.",
                batch.id
            );
        }
    };
    format!(
        "Verification batch {} ended ({ended}) without landing.{found} Members that did not land are verified from the queue.",
        batch.id
    )
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

pub(super) fn outcome_detail(outcome: &VerificationOutcome) -> String {
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
