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
                &format!("Managed landing remains held for native observation: {error}"),
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
    let completed = completed?;
    settled?;
    cost::check(&active)?;
    Ok(if completed {
        TickResult::Completed
    } else {
        TickResult::RetryLater
    })
}
