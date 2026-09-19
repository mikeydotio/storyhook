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
