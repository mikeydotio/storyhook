//! Exact native lifetime across managed landing preflight, spawn and drain.
use super::*;
use crate::service::integration_recovery::IntegrationLandingClaim;

pub(super) struct Operation<'a> {
    claim: &'a IntegrationLandingClaim,
    live: LiveOwner,
}
struct LiveOwner {
    active: ActiveVerification,
    cancellation: Cancellation,
    deadline: Instant,
}
impl<'a> Operation<'a> {
    pub(super) fn new(
        activity: &VerificationActivity,
        claim: &'a IntegrationLandingClaim,
    ) -> Result<Self, AppError> {
        let candidate = claim.candidate();
        let (deadline, cancellation) = claim.operation_lifetime();
        let active = activity
            .active_for(candidate.project)
            .ok_or_else(|| AppError::Validation("managed landing has no central guard".into()))?;
        if active.story_id != candidate.story_id
            || active.generation != candidate.verifying_generation
            || active.mode != VerificationMode::Gated
            || active.attempt_id != claim.certification().attempt
        {
            return Err(AppError::Validation(
                "managed landing lost its exact central gate owner".into(),
            ));
        }
        let operation = Self {
            claim,
            live: LiveOwner {
                active,
                cancellation: cancellation.clone(),
                deadline,
            },
        };
        operation.validate(activity)?;
        Ok(operation)
    }
    pub(super) fn id(&self) -> &str {
        self.claim.id()
    }
    pub(super) fn repository(&self) -> &str {
        &self.claim.publication().original.repository
    }
    pub(super) fn deadline(&self) -> Instant {
        self.live.deadline
    }
    pub(super) fn validate(&self, activity: &VerificationActivity) -> Result<(), AppError> {
        self.live
            .validate_after(activity, || self.claim.validate_custody())
    }
    pub(super) fn take_request(&self) -> Result<(), AppError> {
        if !self.claim.take_request()? {
            return Err(AppError::Validation(
                "managed merge request was already consumed; observe without retrying".into(),
            ));
        }
        Ok(())
    }
}
impl LiveOwner {
    fn validate_after(
        &self,
        activity: &VerificationActivity,
        custody: impl FnOnce() -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        self.validate(activity)?;
        custody()?;
        self.validate(activity)
    }
    fn validate(&self, activity: &VerificationActivity) -> Result<(), AppError> {
        if Instant::now() >= self.deadline
            || self.cancellation.is_cancelled()
            || activity.active_for(self.active.project).as_ref() != Some(&self.active)
            || !self
                .cancellation
                .same_owner(&activity.cancellation_for(self.active.project))
        {
            return Err(AppError::Validation(
                "managed landing original deadline, cancellation or central slot changed".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate() -> VerificationCandidate {
        VerificationCandidate {
            blocked_by: vec![],
            landing_pending: true,
            project: ProjectId::new(1),
            project_slug: "fixture".into(),
            story_id: "SH-1".into(),
            title: "managed landing".into(),
            priority: crate::domain::Priority::Low,
            created_at: "2026-10-08T00:00:00Z".into(),
            verifying_since: None,
            verifying_generation: Some(GlobalSeq::new(1)),
            blocking_revision: None,
            human_only_revision: None,
            checkout: PathBuf::from("/unused-managed-fixture"),
            cleanup_lease: None,
            pull_request: Err(VerificationProblem::MissingPullRequest),
        }
    }
    #[test]
    fn managed_landing_rechecks_cancellation_after_native_custody() {
        let activity = VerificationActivity::new();
        let guard = activity.acquire(&candidate(), "2026-10-08T00:00:00Z".into());
        let owner = LiveOwner {
            active: guard.active.clone(),
            cancellation: guard.cancellation.clone(),
            deadline: Instant::now()
                + storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
        };
        assert!(
            owner
                .validate_after(&activity, || {
                    guard.cancellation.cancel();
                    Ok(())
                })
                .is_err()
        );
    }
    #[test]
    fn managed_landing_refuses_replacement_slot_after_preflight() {
        let activity = VerificationActivity::new();
        let candidate = candidate();
        let guard = activity.acquire(&candidate, "2026-10-08T00:00:00Z".into());
        let owner = LiveOwner {
            active: guard.active.clone(),
            cancellation: guard.cancellation.clone(),
            deadline: Instant::now()
                + storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
        };
        owner.validate(&activity).unwrap();
        drop(guard);
        let replacement = activity.acquire(&candidate, "2026-10-08T00:00:01Z".into());
        assert!(!replacement.is_cancelled());
        assert!(owner.validate_after(&activity, || Ok(())).is_err());
        assert!(
            !replacement.is_cancelled(),
            "old operation cancelled replacement owner"
        );
    }
    #[test]
    fn managed_landing_expired_preflight_does_not_renew_deadline() {
        let activity = VerificationActivity::new();
        let guard = activity.acquire(&candidate(), "2026-10-08T00:00:00Z".into());
        let mut owner = LiveOwner {
            active: guard.active.clone(),
            cancellation: guard.cancellation.clone(),
            deadline: Instant::now()
                + storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
        };
        owner.validate(&activity).unwrap();
        // Logical expiration at the exact boundary, without scheduler timing.
        owner.deadline = Instant::now();
        assert!(
            owner
                .validate_after(&activity, || panic!("expired operation reached custody"))
                .is_err()
        );
    }
}
