//! Returning a story to its agent for repair: the transition and diagnosis,
//! then delivery into the story's own pane (SH-650), and the RED diagnosis a
//! failed gate returns with.
//!
//! Delivery goes through a [`ReturnTransport`]: the verifier's own actuator
//! for the story its attempt owns, or a batch member's own workspace lock for
//! a culprit that bisection found (SH-833), because the story helper refuses
//! a lock that belongs to another story.

use super::*;

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

/// The RED diagnosis for a merge tree that failed its gate. `found_by`
/// names the verification batch whose bisection found the story (SH-833);
/// without it the text is the single-story diagnosis.
pub(super) fn red_diagnosis(
    candidate: &VerificationCandidate,
    tree: &str,
    gate: &str,
    log: &str,
    detail: &str,
    found_by: Option<&str>,
) -> String {
    let found_by = found_by.map_or_else(String::new, |found_by| format!(" {found_by}"));
    format!(
        "CENTRAL VERIFICATION RED — merge tree `{tree}` failed `{gate}`. Full log: `{log}`.{found_by} Fix the branch in its worktree. Run new and impacted tests. Commit the work. Move {} back to verifying. {}.\n\n{}",
        candidate.story_id,
        push_promise(candidate.cleanup_lease.is_some(), false),
        crate::text_lint::quote_evidence(detail)
    )
}

/// Hands a returned story back to its agent: the transition and the
/// diagnosis comment, then delivery into the dispatched pane.
///
/// `Applied(true)` means remediation is under way in the story's own window —
/// the paste landed, or the agent was absent and a resume re-dispatch of the
/// same story into the same window and worktree succeeded (SH-650, decision
/// D-E of `docs/spec/verification-workflow.md`). `Applied(false)` means the
/// story is parked with `awaiting`: only when the re-dispatch itself was
/// refused, or the refusal was not evidence of absence. The Conflict arm holds
/// the queue on `true` and releases it on `false`, so the hold applies whether
/// or not the FIRST paste landed, and never waits for a resubmission nobody
/// will make.
///
/// `reservation` names why `owner` keeps its slot once the return takes its
/// generation out of the queue (SH-768): through delivery, and on a conflict
/// through the wait that follows. It is declared before that write and kept
/// only if the write applied.
pub(super) fn return_for_repair<S: Store, A: VerificationActuator>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    actuator: &A,
    candidate: &VerificationCandidate,
    diagnosis: &str,
    owner: &VerificationGuard,
    reservation: ReservationReason,
) -> Result<GenerationWrite<bool>, AppError> {
    let cancellation = &owner.cancellation;
    if cancellation.is_cancelled() || !queue.human_permits(candidate)? {
        return Ok(GenerationWrite::Applied(false));
    }
    let pending = owner.reserve(reservation, ctx.now());
    if matches!(
        queue.record_generation_returned(ctx, candidate, diagnosis)?,
        GenerationWrite::Superseded
    ) {
        return Ok(GenerationWrite::Superseded);
    }
    pending.retire();
    deliver_return(
        queue,
        ctx,
        &SlotTransport(actuator),
        candidate,
        diagnosis,
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
