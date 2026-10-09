//! The only managed certificate constructor consumes the actual central
//! certify-only runner result, settled cost execution and fresh native inputs.
//! It does not accept public JSON or infer certification from accounting alone.
use super::*;
use crate::service::integration_recovery::{
    IntegrationGateClaim, IntegrationGateInputsEvidence, IntegrationOwnerService,
    NativeIntegrationGateInputs,
};

pub(crate) struct NativeIntegrationCertification {
    pub(crate) execution: String,
    pub(crate) certification: crate::domain::landing::VerifiedSubmission,
    inputs: NativeIntegrationGateInputs,
}
impl NativeIntegrationCertification {
    pub(crate) fn inputs(&self) -> &IntegrationGateInputsEvidence {
        self.inputs.evidence()
    }
    pub(crate) fn validate_for(&self, claim: &IntegrationGateClaim) -> Result<(), AppError> {
        self.inputs.validate_for(claim)?;
        self.certification.validate()?;
        if self.certification.head != claim.publication().commit
            || self.certification.tree != claim.publication().tree
            || self.execution.is_empty()
        {
            return Err(AppError::Validation(
                "managed gate result differs from native owned inputs".into(),
            ));
        }
        Ok(())
    }
}

// Owner-only transaction tests substitute the native runner boundary. No
// production constructor accepts caller JSON or unowned accounting receipts.
#[cfg(test)]
pub(crate) fn fixture_certification(
    execution: String,
    certification: crate::domain::landing::VerifiedSubmission,
    inputs: NativeIntegrationGateInputs,
) -> NativeIntegrationCertification {
    NativeIntegrationCertification {
        execution,
        certification,
        inputs,
    }
}

pub(super) enum ManagedGateResult {
    Certified(Box<NativeIntegrationCertification>),
    Uncertified(Box<VerificationOutcome>),
}

// Only the serialized central worker can call this; the borrowed guard stays
// alive through native process settlement and the post-run input observation.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_with_inputs<S: Store>(
    store: &S,
    env: &Environment,
    bus: &ChangeBus,
    actuator: &impl VerificationActuator,
    active: &VerificationGuard,
    service: &IntegrationOwnerService<'_, S>,
    claim: &IntegrationGateClaim,
    observe: impl FnOnce(
        &IntegrationOwnerService<'_, S>,
        &IntegrationGateClaim,
        Instant,
        &Cancellation,
    ) -> Result<NativeIntegrationGateInputs, AppError>,
) -> Result<ManagedGateResult, AppError> {
    let inputs = claim.inputs().ok_or_else(|| {
        AppError::Validation("managed gate lacks a consumed native input proof".into())
    })?;
    let candidate = claim.candidate();
    if active.registry.active_for(candidate.project).as_ref() != Some(&active.active)
        || active.active.project != candidate.project
        || active.active.story_id != candidate.story_id
        || active.active.generation != candidate.verifying_generation
        || active.active.attempt_id != claim.attempt()
        || active.active.mode != VerificationMode::Gated
        || !claim.owns_cancellation(&active.cancellation)
        || !service.gate_permitted(claim)?
    {
        return Err(AppError::Validation(
            "managed gate does not own the original live central slot".into(),
        ));
    }
    let reference = parse_pr_url(&claim.publication().pull_request)?;
    let managed = PrLink {
        owner: reference.owner,
        repo: reference.repo,
        number: reference.number,
        url: claim.publication().pull_request.clone(),
        close_on_merge: false,
        status: "open".into(),
        linked_at: env.now(),
        last_checked_at: None,
    };
    let subscription = bus.subscribe();
    let (execution, outcome, current) = cost::execute(
        store,
        env,
        active,
        candidate,
        crate::store::GateExecutionPurpose::Gate,
        crate::store::GateInputs {
            head: Some(inputs.publication.commit.clone()),
            base: Some(inputs.current_base.clone()),
            tree: Some(inputs.tree.clone()),
            ..Default::default()
        },
        vec![cost::submission(candidate)],
        |execution| {
            let (outcome, current) = observation::observe_during(
                &subscription,
                &active.cancellation,
                &active.cancellation,
                &candidate.project_slug,
                || service.gate_permitted(claim),
                || actuator.verify_integration(candidate, &managed, &active.cancellation),
            );
            (execution.id.clone(), outcome, current)
        },
        |(_, outcome, current)| match current {
            Ok(true) => Ok(Some(outcome.clone())),
            Ok(false) => Ok(None),
            Err(error) => Err(error.to_string()),
        },
    )?;
    if !current? || !service.gate_permitted(claim)? {
        return Ok(ManagedGateResult::Uncertified(Box::new(
            VerificationOutcome::Cancelled,
        )));
    }
    let VerificationOutcome::Certified {
        head, tree, gate, ..
    } = &outcome
    else {
        return Ok(ManagedGateResult::Uncertified(Box::new(outcome)));
    };
    // Native control observation is separately bounded; the progressing test
    // runner above retains its existing idle supervision with no new total cap.
    let deadline = Instant::now() + env.subprocess_bound(Duration::from_secs(30));
    let observed = observe(service, claim, deadline, &active.cancellation)?;
    if observed.evidence() != inputs {
        return Err(AppError::Validation(
            "managed gate inputs changed before certification consumption".into(),
        ));
    }
    let certified = NativeIntegrationCertification {
        execution,
        certification: crate::domain::landing::VerifiedSubmission {
            head: head.clone(),
            tree: tree.clone(),
            gate: gate.clone(),
        },
        inputs: observed,
    };
    certified.validate_for(claim)?;
    Ok(ManagedGateResult::Certified(Box::new(certified)))
}
