//! Private pinned-input admission under the exact live verifier authority.

use super::*;
use crate::service::project_recovery::{ProjectRecoveryService, RepairAdmission, RepairInput};

impl VerificationActivity {
    pub(crate) fn admit_repair(
        &self,
        ctx: &Ctx<'_, impl Store>,
        story: &str,
        attempt: &str,
        generation: i64,
        input: &RepairInput,
    ) -> Result<RepairAdmission, AppError> {
        // Registry before store, as for cancellation and verifier controls. Never
        // reconstruct a candidate that could inherit authority after acquisition.
        let slots = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots
            .get(&ctx.project())
            .filter(|slot| {
                !slot.cancellation.is_cancelled()
                    && generation > 0
                    && slot.active.story_id == story
                    && slot.active.attempt_id == attempt
                    && slot.active.generation == Some(GlobalSeq::new(generation))
            })
            .ok_or_else(|| {
                AppError::Validation(
                    "private repair admission does not match the live uncancelled verifier owner"
                        .into(),
                )
            })?;
        ProjectRecoveryService::new(ctx).admit_repair(&slot.candidate, attempt, input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_wire_never_grants_disposition_through_uncertain_cleanup() {
        let wire = serde_json::json!({"result":"repair-deferred", "recovery_id":"recovery-1", "reason":"unchanged-input"});
        let parsed: WireOutcome = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(
            parsed.into_outcome(),
            VerificationOutcome::RepairDeferred { .. }
        ));
        let interrupted = cleanup::interrupted_outcome(
            wire.to_string().as_bytes(),
            "capture timed out",
            std::path::Path::new("/source"),
        );
        assert!(
            matches!(interrupted, Some(VerificationOutcome::InfrastructureFailure { detail, .. }) if detail.contains("recovery-1"))
        );
        let mut unsettled = wire.clone();
        unsettled["cleanup_failure"] = serde_json::json!({"phase":"outer census", "detail":"survivor", "owner":"owner", "worktree":"worktree", "disposition":"permanent"});
        let parsed: WireOutcome = serde_json::from_value(unsettled).unwrap();
        assert!(matches!(
            parsed.into_outcome(),
            VerificationOutcome::InfrastructureFailure { .. }
        ));
        let mut invalid = wire.clone();
        invalid["reason"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<WireOutcome>(invalid).is_err());
        let mut missing = wire;
        missing["recovery_id"] = serde_json::json!("");
        assert!(matches!(
            serde_json::from_value::<WireOutcome>(missing)
                .unwrap()
                .into_outcome(),
            VerificationOutcome::InfrastructureFailure { .. }
        ));
    }
}
