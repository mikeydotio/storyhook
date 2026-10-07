//! Apply native causal evidence, then deliver only the proved repair scope.
//! Unknown or administrative failures retain a generation-bound diagnosis hold.

use super::*;

/// Hold an unproved failure without sending a repair assignment to the implementer.
pub(super) fn hold_for_attribution<S: Store>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    check: &str,
    cause: crate::service::attribution::FailureCause,
    detail: &str,
    owner: &VerificationGuard,
) -> Result<GenerationWrite<()>, AppError> {
    if owner.is_cancelled() {
        return Ok(GenerationWrite::Superseded);
    }
    let pending = owner.reserve(ReservationReason::Attribution, ctx.now());
    let result = queue.record_generation_held(
        ctx,
        candidate,
        &owner.active.attempt_id,
        check,
        cause,
        detail,
    )?;
    if matches!(result, GenerationWrite::Applied(())) {
        pending.retire();
    }
    Ok(result)
}

/// How a returned story's diagnosis reaches its agent.
pub(super) trait ReturnTransport {
    /// Pastes `message` into the story's agent pane.
    fn notify(
        &self,
        candidate: &VerificationCandidate,
        message: &str,
    ) -> Result<NotifyDelivery, AppError>;
    /// Re-dispatches the story into its own window and worktree with the
    /// resume clause.
    fn redispatch(
        &self,
        candidate: &VerificationCandidate,
        plan: &ResumePlan,
    ) -> Result<(), AppError>;
}

/// Delivery through the actuator, for the story the attempt owns: the
/// actuator lends the slot's own workspace lock.
pub(super) struct SlotTransport<'a, A>(pub(super) &'a A);

impl<A: VerificationActuator> ReturnTransport for SlotTransport<'_, A> {
    fn notify(
        &self,
        candidate: &VerificationCandidate,
        message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        self.0.notify(candidate, message)
    }

    fn redispatch(
        &self,
        candidate: &VerificationCandidate,
        plan: &ResumePlan,
    ) -> Result<(), AppError> {
        self.0.redispatch(candidate, plan)
    }
}

/// Apply a settled native causal capability before delivering its exact repair scope.
/// The slot reservation retains ownership through metadata refresh and delivery.
pub(super) fn return_for_repair<S: Store, A: VerificationActuator>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    actuator: &A,
    candidate: &VerificationCandidate,
    proof: &crate::service::attribution::CausalReturnEvidence,
    owner: &VerificationGuard,
    reservation: ReservationReason,
) -> Result<GenerationWrite<bool>, AppError> {
    let cancellation = &owner.cancellation;
    if cancellation.is_cancelled() || !queue.human_permits(candidate)? {
        return Ok(GenerationWrite::Applied(false));
    }
    let pending = owner.reserve(reservation, ctx.now());
    let head = actuator.current_pr_head(candidate);
    if !matches!(&head, Ok(head) if head == proof.submitted_head()) {
        let detail = match head {
            Ok(head) => format!(
                "current PR head {head} differs from proved head {}",
                proof.submitted_head()
            ),
            Err(error) => error.to_string(),
        };
        queue.upsert_generation_comment(ctx, candidate, "CENTRAL VERIFICATION HEAD HELD", &format!("CENTRAL VERIFICATION HEAD HELD — current head cannot authorize this causal return. No repair is assigned.\n\n{}", crate::text_lint::quote_evidence(&detail)), None)?;
        pending.retire();
        return Ok(GenerationWrite::Applied(false));
    }
    if cancellation.is_cancelled() {
        return Ok(GenerationWrite::Superseded);
    }
    if crate::service::project_recovery::ProjectRecoveryService::new(ctx)
        .return_proven_repair(candidate, proof)?
    {
        pending.retire();
        return Ok(GenerationWrite::Applied(true));
    }
    if !queue.record_causal_return(ctx, candidate, proof)? {
        return Ok(GenerationWrite::Superseded);
    }
    let diagnosis = proof.diagnosis();
    pending.retire();
    deliver_return(
        queue,
        ctx,
        &SlotTransport(actuator),
        candidate,
        &diagnosis,
        cancellation,
    )
}

/// Delivers a return that is already recorded: pastes the diagnosis, and if
/// the agent is absent re-dispatches it with the resume clause and pastes
/// again, leaving a trail comment for each step. Answers as
/// [`return_for_repair`] does.
pub(super) fn deliver_return<S: Store>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    transport: &dyn ReturnTransport,
    candidate: &VerificationCandidate,
    diagnosis: &str,
    cancellation: &Cancellation,
) -> Result<GenerationWrite<bool>, AppError> {
    let activity_context = format!("project={} {}", candidate.project_slug, candidate.story_id);
    let delivered = transport.notify(candidate, diagnosis);
    if cancellation.is_cancelled() || !queue.human_permits(candidate)? {
        return Ok(GenerationWrite::Applied(false));
    }
    let absent = match delivered {
        Ok(NotifyDelivery::Delivered) => return Ok(GenerationWrite::Applied(true)),
        Ok(NotifyDelivery::AgentAbsent { reason, detail }) => format!("{detail} ({reason})"),
        Err(error) => {
            let reason = format!("verification remediation could not reach its agent: {error}");
            return park(queue, ctx, candidate, &reason);
        }
    };
    // A re-dispatch launches an unattended session, and no automation
    // launches one for a story left for a person (SH-837, amending D-E):
    // the diagnosis waits in the story's comments instead. `human-only`
    // never reaches here; `human_permits` ended the return above.
    if let Some(label) = queue.reserved_label(candidate)? {
        let reason = format!(
            "verification remediation will not re-dispatch its agent: {} carries `{label}`, so it is left for a person. Read the diagnosis in the story's comments (after: {absent})",
            candidate.story_id
        );
        return park(queue, ctx, candidate, &reason);
    }
    // The trail is written BEFORE the respawn so a daemon that dies inside it
    // leaves a story that says what was attempted, not one that merely sits
    // in-progress with a dead pane (SH-306).
    comment_once(
        ctx,
        candidate,
        &format!(
            "{VERIFICATION_RESUME_PREFIX} re-dispatching {} into its own window and worktree with the resume clause.\n\n{}",
            candidate.story_id,
            crate::text_lint::quote_evidence(&absent)
        ),
    )?;
    let plan = resume_plan(ctx.store(), candidate)?;
    super::super::activity::emit(
        "INFO",
        "verifier",
        "event",
        &activity_context,
        &format!("agent absent ({absent}); re-dispatching with {plan:?}"),
    );
    let redispatched = transport.redispatch(candidate, &plan);
    if cancellation.is_cancelled() || !queue.human_permits(candidate)? {
        return Ok(GenerationWrite::Applied(false));
    }
    if let Err(error) = redispatched {
        super::super::activity::emit(
            "ERROR",
            "verifier",
            "event",
            &activity_context,
            &format!("resume re-dispatch refused: {error}"),
        );
        let reason = format!(
            "verification remediation could not re-dispatch its agent: {error} (after: {absent})"
        );
        return park(queue, ctx, candidate, &reason);
    }
    // The agent is live under the resume charter, which begins by reading this
    // story's comments — where the diagnosis already is. A paste that fails
    // here is recorded, never a hard stop (D-E: `awaiting` only when the
    // re-dispatch itself is refused).
    let delivered = transport.notify(candidate, diagnosis);
    if cancellation.is_cancelled() || !queue.human_permits(candidate)? {
        return Ok(GenerationWrite::Applied(false));
    }
    match delivered {
        Ok(NotifyDelivery::Delivered) => {}
        Ok(NotifyDelivery::AgentAbsent { reason, detail }) => comment_once(
            ctx,
            candidate,
            &format!(
                "{VERIFICATION_RESUME_PREFIX} re-dispatched, but the diagnosis could not be pasted afterwards. Read the diagnosis in the previous comment.\n\n{}",
                crate::text_lint::quote_evidence(&format!("{detail} ({reason})"))
            ),
        )?,
        Err(error) => comment_once(
            ctx,
            candidate,
            &format!(
                "{VERIFICATION_RESUME_PREFIX} re-dispatched, but the diagnosis could not be pasted afterwards. Read the diagnosis in the previous comment.\n\n{}",
                crate::text_lint::quote_evidence(&error.to_string())
            ),
        )?,
    }
    Ok(GenerationWrite::Applied(true))
}

pub(super) fn park<S: Store>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    reason: &str,
) -> Result<GenerationWrite<bool>, AppError> {
    if matches!(
        queue.set_generation_awaiting(ctx, candidate, reason)?,
        GenerationWrite::Superseded
    ) {
        return Ok(GenerationWrite::Superseded);
    }
    Ok(GenerationWrite::Applied(false))
}

/// Derives the resume plan for `candidate` from the store (SH-650).
///
/// A story held by a live lane — `Dispatching` or `Working`, never `Idle` or
/// `Quarantined` — of a live engine run in the candidate's project is
/// re-dispatched as that lane: the run's provider options and `--full-auto`.
/// A quarantined lane is one the engine has already given up on, so passing
/// its identity would be a lie about who observes the window. Everything
/// else (an attended dispatch, a finished run) is an ordinary autonomous
/// resume whose provider the helper reads from the dispatch's own record.
/// Public for store-backed integration tests.
pub fn resume_plan(
    store: &impl Store,
    candidate: &VerificationCandidate,
) -> Result<ResumePlan, AppError> {
    let plan = store.read(|tx| {
        for run in tx.live_engine_runs()? {
            if run.project_slug != candidate.project_slug {
                continue;
            }
            let held = tx.engine_lanes(&run.id)?.into_iter().any(|lane| {
                matches!(
                    lane.state,
                    EngineLaneState::Dispatching | EngineLaneState::Working
                ) && lane.story_id.as_deref() == Some(candidate.story_id.as_str())
            });
            if held {
                return Ok(ResumePlan {
                    agent: Some(run.agent),
                    model: run.model.clone(),
                    effort: run.effort.clone(),
                    fast: run.speed == Some(EngineSpeed::Fast),
                    full_auto: true,
                });
            }
        }
        Ok(ResumePlan::default())
    })?;
    Ok(plan)
}
