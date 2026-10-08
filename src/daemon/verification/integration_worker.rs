//! Managed integration uses the existing serialized per-project verifier.
//! Restart can observe an exact pending merge, never replay its old effects.
use super::*;
use crate::service::integration_recovery::{
    IntegrationLandingObservation, IntegrationOwnerService, NativeIntegrationLanded,
    observe_landed_owned,
};

pub(super) fn reconcile<S: Store>(
    store: &S,
    env: &Environment,
    activity: &VerificationActivity,
    inflight: &InFlight,
    project: ProjectId,
) -> Result<Option<TickResult>, AppError> {
    let intents = store.read(|tx| tx.landing_intents())?;
    for intent in intents
        .into_iter()
        .filter(|i| i.project == project && i.certification.integration().is_some())
    {
        let owner = &intent.certification.integration().expect("filtered").owner;
        let result = reconcile_one_with(
            store,
            env,
            activity,
            inflight,
            project,
            owner,
            |service, query, deadline, cancellation| {
                observe_landed_owned(service, query, deadline, cancellation)
            },
        );
        match result {
            Ok(TickResult::Completed) => return Ok(Some(TickResult::Completed)),
            Ok(_) => {}
            Err(error) => super::super::activity::emit(
                "ERROR",
                "verifier",
                "event",
                &format!("project={project} managed-owner={owner}"),
                &format!(
                    "Managed landing observation or owned cleanup remains incomplete: {error}"
                ),
            ),
        }
        // A still-open or uncertain managed PR cannot starve unrelated work.
    }
    Ok(None)
}

// Test fixtures may replace only the native remote observation. Actual central
// admission, Store fences, completion and new private resource settlement remain.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconcile_one_with<S: Store>(
    store: &S,
    env: &Environment,
    activity: &VerificationActivity,
    inflight: &InFlight,
    project: ProjectId,
    owner: &str,
    observe: impl FnOnce(
        &IntegrationOwnerService<'_, S>,
        IntegrationLandingObservation,
        Instant,
        &Cancellation,
    ) -> Result<NativeIntegrationLanded, AppError>,
) -> Result<TickResult, AppError> {
    let checkout = store
        .read(|tx| tx.checkout_path(project))?
        .ok_or_else(|| AppError::Validation("managed observation checkout disappeared".into()))?;
    let ctx = Ctx::new(store, project, checkout, env.clone()).no_hooks(true);
    let service = IntegrationOwnerService::new(&ctx);
    let (_, state) = service.show(owner)?;
    let candidate = &state.candidate;
    let lifecycle = inflight.enter();
    let at = env.now();
    name_verification(&lifecycle, candidate, &at);
    let Some(active) = activity.admit(
        store,
        env,
        candidate,
        at,
        Some(ReservationReason::IntegrationObservation),
    )?
    else {
        return Ok(TickResult::Stopped);
    };
    let deadline = Instant::now() + env.subprocess_bound(Duration::from_secs(30));
    let query = service.observe_landing(owner, deadline, &active.cancellation)?;
    let native = observe(&service, query, deadline, &active.cancellation)?;
    let completed = service.complete_landing(&native);
    // Even revoked/expired proof still owns cleanup of its newly allocated
    // observation repo. Errors retain and name residue, never adopt old paths.
    let settled = native.settle();
    let completed = match (completed, settled) {
        (Ok(completed), Ok(())) => completed,
        (Err(error), Ok(())) => return Err(error),
        (Ok(completed), Err(cleanup)) => {
            return Err(AppError::Storage(format!(
                "managed completion recorded={completed}; {cleanup}"
            )));
        }
        (Err(completion), Err(cleanup)) => {
            return Err(AppError::Storage(format!(
                "managed completion refused: {completion}; owned observation cleanup failed: {cleanup}"
            )));
        }
    };
    cost::check(&active)?;
    Ok(if completed {
        TickResult::Completed
    } else {
        TickResult::RetryLater
    })
}

// This is only a cheap opt-in hint. The actual immutable base policy is always
// freshly inspected by the native proposal before any owned external effect.
fn enabled_hint(candidate: &VerificationCandidate) -> bool {
    crate::service::project::read_pointer(&candidate.checkout)
        .ok()
        .flatten()
        .and_then(|p| p.integration)
        .and_then(|v| v.get("enabled").and_then(toml::Value::as_bool))
        == Some(true)
}
#[allow(clippy::too_many_arguments)]
pub(super) fn start_one<S: Store>(
    store: &S,
    env: &Environment,
    actuator: &impl VerificationActuator,
    activity: &VerificationActivity,
    inflight: &InFlight,
    project: ProjectId,
    bus: &ChangeBus,
) -> Result<Option<TickResult>, AppError> {
    use crate::service::integration_recovery as integration;
    let subjects = store.read(|tx| integration::pending_subjects(tx, project))?;
    for subject in subjects {
        if !enabled_hint(&subject.candidate) {
            continue;
        }
        let result = start_one_with(
            store,
            env,
            activity,
            inflight,
            bus,
            &subject,
            |service, active| run_native(store, env, bus, actuator, service, active, &subject),
        );
        match result {
            Ok(TickResult::Completed) => return Ok(Some(TickResult::Completed)),
            Ok(_) => {}
            Err(error) => super::super::activity::emit(
                "ERROR",
                "verifier",
                "event",
                &format!("project={project} story={}", subject.candidate.story_id),
                &format!("Managed integration remains held: {error}"),
            ),
        }
        // A held/unsupported original subject must not hide unrelated work.
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_one_with<S: Store>(
    store: &S,
    env: &Environment,
    activity: &VerificationActivity,
    inflight: &InFlight,
    bus: &ChangeBus,
    subject: &crate::service::integration_recovery::PendingIntegration,
    run: impl FnOnce(
        &IntegrationOwnerService<'_, S>,
        &VerificationGuard,
    ) -> Result<TickResult, AppError>,
) -> Result<TickResult, AppError> {
    use crate::service::integration_recovery as integration;
    let candidate = &subject.candidate;
    if !store.read(|tx| integration::candidate_permitted(tx, candidate))? {
        return Ok(TickResult::Stopped);
    }
    let lifecycle = inflight.enter();
    let at = env.now();
    name_verification(&lifecycle, candidate, &at);
    let Some(active) = activity.admit(
        store,
        env,
        candidate,
        at,
        Some(ReservationReason::Integration),
    )?
    else {
        return Ok(TickResult::Stopped);
    };
    if active.active.mode != VerificationMode::Gated {
        return Ok(TickResult::Stopped);
    }
    let ctx = Ctx::new(
        store,
        candidate.project,
        candidate.checkout.clone(),
        env.clone(),
    )
    .no_hooks(true);
    let service = IntegrationOwnerService::new(&ctx);
    let subscription = bus.subscribe();
    let (result, current) = observation::observe_during(
        &subscription,
        &active.cancellation,
        &active.cancellation,
        &candidate.project_slug,
        || {
            store
                .read(|tx| integration::candidate_permitted(tx, candidate))
                .map_err(Into::into)
        },
        || run(&service, &active),
    );
    let result = result?;
    if !current? && result != TickResult::Completed {
        return Ok(TickResult::Stopped);
    }
    cost::check(&active)?;
    Ok(result)
}

fn proposed<T>(
    candidate: &VerificationCandidate,
    head: &str,
    env: &Environment,
    cancel: &Cancellation,
    run: impl FnOnce(
        &crate::service::integration_recovery::BoundIntegrationProposal,
        Instant,
    ) -> Result<T, AppError>,
) -> Result<T, AppError> {
    use crate::service::integration_recovery::{BoundInspection, inspect_submission};
    let deadline = Instant::now() + env.subprocess_bound(Duration::from_secs(30));
    let proof=match inspect_submission(candidate,head,env,deadline,cancel.clone())? {
        BoundInspection::Proposed(proof)=>proof,
        BoundInspection::Held(reason)=>return Err(AppError::Validation(reason)),
        BoundInspection::Clean=>return Err(AppError::Validation("original conflict no longer needs integration; retain its diagnostic until fresh ordinary gate readmission is proved".into())),
    };
    let result = run(&proof, deadline);
    let settled = proof.settle();
    match (result, settled) {
        (Ok(value), Ok(_)) => Ok(value),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(AppError::Storage(format!(
            "{error}; native proposal cleanup failed: {cleanup}"
        ))),
    }
}

fn owned<T>(value: Option<T>, phase: &str) -> Result<T, AppError> {
    value.ok_or_else(|| {
        AppError::Validation(format!(
            "managed {phase} authority changed; retain without replay"
        ))
    })
}
#[allow(clippy::too_many_arguments)]
fn run_native<S: Store>(
    store: &S,
    env: &Environment,
    bus: &ChangeBus,
    actuator: &impl VerificationActuator,
    service: &IntegrationOwnerService<'_, S>,
    active: &VerificationGuard,
    subject: &crate::service::integration_recovery::PendingIntegration,
) -> Result<TickResult, AppError> {
    use crate::service::integration_recovery as integration;
    let candidate = &subject.candidate;
    let cancel = &active.cancellation;
    let mut owner = None;
    let run = (|| {
        let assembled = proposed(
            candidate,
            &subject.retained_head,
            env,
            cancel,
            |proof, deadline| {
                let record =
                    service.reserve(candidate, &subject.attribution, &subject.component, proof)?;
                owner = Some(record.id.clone());
                let claim = owned(
                    service.claim_assembly(&record.id, record.revision, proof)?,
                    "assembly",
                )?;
                let native = integration::assemble_owned(service, &claim, proof, deadline, cancel)?;
                owned(
                    service.accept_assembly(claim, native, proof)?,
                    "assembly acceptance",
                )
            },
        )?;
        let published = proposed(
            candidate,
            &subject.retained_head,
            env,
            cancel,
            |proof, deadline| {
                let mut claim = owned(service.claim_publication(assembled, proof)?, "publication")?;
                let native =
                    integration::publish_owned(service, &mut claim, proof, deadline, cancel)?;
                owned(
                    service.accept_publication(claim, native, proof)?,
                    "publication acceptance",
                )
            },
        )?;
        let (claim, deadline) = proposed(
            candidate,
            &subject.retained_head,
            env,
            cancel,
            |proof, deadline| {
                Ok((
                    owned(
                        service.claim_gate(
                            published,
                            proof,
                            &active.active.attempt_id,
                            deadline,
                            cancel,
                        )?,
                        "gate admission",
                    )?,
                    deadline,
                ))
            },
        )?;
        let inputs = integration::observe_gate_inputs(service, &claim, deadline, cancel)?;
        let running = owned(service.start_gate(claim, inputs)?, "gate start")?;
        let native = match super::integration_gate::run(
            store, env, bus, actuator, active, service, &running,
        )? {
            super::integration_gate::ManagedGateResult::Certified(native) => native,
            super::integration_gate::ManagedGateResult::Uncertified(outcome) => {
                return Err(AppError::Validation(format!(
                    "managed gate did not certify the exact tree: {outcome:?}"
                )));
            }
        };
        let certified = owned(
            service.accept_gate(running, native)?,
            "certificate consumption",
        )?;
        let mut landing = proposed(
            candidate,
            &subject.retained_head,
            env,
            cancel,
            |proof, _| {
                owned(
                    service.claim_landing(certified, proof)?,
                    "landing admission",
                )
            },
        )?;
        if !service.claim_landing_effect(&mut landing)? {
            return Err(AppError::Validation(
                "managed merge effect was not claimed".into(),
            ));
        }
        let subscription = bus.subscribe();
        let (outcome, current) = observation::observe_during(
            &subscription,
            cancel,
            cancel,
            &candidate.project_slug,
            || service.landing_permitted(&landing),
            || actuator.land_integration(&landing),
        );
        // No helper JSON, including Merged, is completion authority. Only a
        // fresh actual remote/Git proof may complete the original submission.
        if !current? {
            return Err(AppError::Validation(format!(
                "managed landing authority revoked; observed helper result {outcome:?}"
            )));
        }
        let deadline = Instant::now() + env.subprocess_bound(Duration::from_secs(30));
        let query = service.observe_landing(landing.id(), deadline, cancel)?;
        let native = integration::observe_landed_owned(service, query, deadline, cancel)?;
        let completion = service.complete_landing(&native);
        let cleanup = native.settle();
        let complete = match (completion, cleanup) {
            (Ok(done), Ok(())) => done,
            (Err(e), Ok(())) => return Err(e),
            (Ok(done), Err(e)) => {
                return Err(AppError::Storage(format!(
                    "managed completion recorded={done}; {e}"
                )));
            }
            (Err(e), Err(c)) => {
                return Err(AppError::Storage(format!(
                    "{e}; native landing proof cleanup failed: {c}"
                )));
            }
        };
        if !complete {
            return Ok(TickResult::RetryLater);
        }
        // Effects returned through quiescent native boundaries while this exact
        // central slot remained held. Only original open assembly custody can
        // be settled; a restart never adopts its historical pathname.
        landing.settle_assembly()?;
        Ok(TickResult::Completed)
    })();
    if let Err(error) = &run
        && let Some(id) = owner
    {
        if let Err(note) = service.note_hold(&id, &error.to_string()) {
            return Err(AppError::Storage(format!(
                "{error}; could not retain managed hold: {note}"
            )));
        }
    }
    run
}
