//! Native observation/transport boundary shared by the worker and its fixtures.
//! All durable ownership, assembly, physical gate accounting and completion stay
//! in the worker. Production has exactly one implementation and no config hook.
use super::*;
use crate::service::integration_recovery as integration;

pub(crate) trait NativeOperations {
    fn inspect(
        &self,
        candidate: &VerificationCandidate,
        head: &str,
        env: &Environment,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<integration::BoundInspection, AppError>;
    fn clean(
        &self,
        candidate: &VerificationCandidate,
        head: &str,
        env: &Environment,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<integration::NativeCleanIntegration, AppError>;
    fn publish<S: Store>(
        &self,
        service: &IntegrationOwnerService<'_, S>,
        claim: &mut integration::PublicationClaim,
        proof: &integration::BoundIntegrationProposal,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<integration::NativePublication, AppError>;
    fn gate_inputs<S: Store>(
        &self,
        service: &IntegrationOwnerService<'_, S>,
        claim: &integration::IntegrationGateClaim,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<integration::NativeIntegrationGateInputs, AppError>;
    fn landed<S: Store>(
        &self,
        service: &IntegrationOwnerService<'_, S>,
        query: integration::IntegrationLandingObservation,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<integration::NativeIntegrationLanded, AppError>;
    fn branch(
        &self,
        env: &Environment,
        assembly: &integration::AssemblyEvidence,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> integration::RetainedBranchObservation;
}

pub(super) struct Native;
impl NativeOperations for Native {
    fn inspect(
        &self,
        c: &VerificationCandidate,
        h: &str,
        e: &Environment,
        d: Instant,
        x: &Cancellation,
    ) -> Result<integration::BoundInspection, AppError> {
        integration::inspect_submission(c, h, e, d, x.clone())
    }
    fn clean(
        &self,
        c: &VerificationCandidate,
        h: &str,
        e: &Environment,
        d: Instant,
        x: &Cancellation,
    ) -> Result<integration::NativeCleanIntegration, AppError> {
        integration::observe_clean_submission(c, h, e, d, x.clone())
    }
    fn publish<S: Store>(
        &self,
        s: &IntegrationOwnerService<'_, S>,
        c: &mut integration::PublicationClaim,
        p: &integration::BoundIntegrationProposal,
        d: Instant,
        x: &Cancellation,
    ) -> Result<integration::NativePublication, AppError> {
        integration::publish_owned(s, c, p, d, x)
    }
    fn gate_inputs<S: Store>(
        &self,
        s: &IntegrationOwnerService<'_, S>,
        c: &integration::IntegrationGateClaim,
        d: Instant,
        x: &Cancellation,
    ) -> Result<integration::NativeIntegrationGateInputs, AppError> {
        integration::observe_gate_inputs(s, c, d, x)
    }
    fn landed<S: Store>(
        &self,
        s: &IntegrationOwnerService<'_, S>,
        q: integration::IntegrationLandingObservation,
        d: Instant,
        x: &Cancellation,
    ) -> Result<integration::NativeIntegrationLanded, AppError> {
        integration::observe_landed_owned(s, q, d, x)
    }
    fn branch(
        &self,
        e: &Environment,
        a: &integration::AssemblyEvidence,
        d: Instant,
        x: &Cancellation,
    ) -> integration::RetainedBranchObservation {
        integration::observe_retained_branch(e, a, d, x)
    }
}
