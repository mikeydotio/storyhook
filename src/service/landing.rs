//! Admission and resolution of durable verifier landing authority.

use super::{Ctx, VerificationCandidate, VerificationQueue};
pub use crate::domain::landing::VerifiedSubmission;
use crate::error::AppError;
use crate::store::{LandingIntent, ReadOps, Store, StoreError, StoryNo, WriteOps};

/// Whether verification may begin an external merge for its exact submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandingAdmission {
    /// This caller acquired new durable authority.
    Admitted(LandingIntent),
    /// Existing authority must be reconciled before another attempt.
    Pending(LandingIntent),
    /// Open dependencies hold this submission without returning it to its author.
    Held(Vec<String>),
    /// Submission identity changed before admission.
    Superseded,
}

impl<S: Store> VerificationQueue<'_, S> {
    /// Atomically checks submission authority and acquires a durable landing intent.
    pub fn begin_landing(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        certification: &VerifiedSubmission,
    ) -> Result<LandingAdmission, AppError> {
        certification.validate()?;
        if ctx.project() != candidate.project {
            return Err(AppError::Validation(
                "landing context belongs to another project".into(),
            ));
        }
        Ok(self.store.write(|tx| {
            if let Some(intent) = tx
                .landing_intents()?
                .into_iter()
                .find(|i| i.project == candidate.project && i.story_id == candidate.story_id)
            {
                return Ok(LandingAdmission::Pending(intent));
            }
            let Some(current) = super::verification::ordered_candidates_for(tx, candidate.project)?
                .into_iter()
                .find(|c| c.project == candidate.project && c.story_id == candidate.story_id)
            else {
                return Ok(LandingAdmission::Superseded);
            };
            if current.verifying_generation != candidate.verifying_generation
                || current.pull_request != candidate.pull_request
                || current.checkout != candidate.checkout
                || current.project_slug != candidate.project_slug
            {
                return Ok(LandingAdmission::Superseded);
            }
            if !current.blocked_by.is_empty() {
                return Ok(LandingAdmission::Held(current.blocked_by));
            }
            let Some(generation) = current.verifying_generation else {
                return Ok(LandingAdmission::Superseded);
            };
            let pr = current
                .pull_request
                .map_err(|problem| StoreError::Validation(problem.message()))?;
            let prefix = super::project_prefix(tx, candidate.project)?;
            let intent = LandingIntent {
                id: uuid::Uuid::new_v4().to_string(),
                project: candidate.project,
                story: StoryNo::parse_id(&prefix, &candidate.story_id)?,
                story_id: candidate.story_id.clone(),
                project_slug: candidate.project_slug.clone(),
                generation,
                pull_request: pr.url,
                checkout: candidate.checkout.clone(),
                certification: certification.clone(),
                created_at: ctx.now(),
            };
            crate::store::landing::validate_intent(tx, &intent)?;
            tx.insert_landing_intent(&intent)?;
            Ok(LandingAdmission::Admitted(intent))
        })?)
    }

    /// Records a conclusive merge and releases its exact durable authority atomically.
    pub fn complete_landing(
        &self,
        ctx: &Ctx<'_, S>,
        intent: &LandingIntent,
        detail: &str,
    ) -> Result<bool, AppError> {
        use crate::domain::{StoryEvent, completion_state};
        if ctx.project() != intent.project {
            return Err(AppError::Validation(
                "landing context belongs to another project".into(),
            ));
        }
        Ok(self.store.write(|tx| {
            if !tx.landing_intents()?.contains(intent) {
                return Ok(false);
            }
            crate::store::landing::validate_intent(tx, intent)?;
            let prefix = super::project_prefix(tx, intent.project)?;
            let row = tx
                .story(intent.project, intent.story)?
                .ok_or_else(|| StoreError::NotFound(intent.story_id.clone()))?;
            let done = completion_state(&tx.states(intent.project)?).ok_or_else(|| {
                StoreError::Validation("project lacks required done state".into())
            })?;
            let states = tx.state_map(intent.project)?;
            let now = ctx.now();
            let comment = format!(
                "{} merge tree `{}` passed `{}` and pull request {} landed. {detail}",
                super::VERIFICATION_GREEN_PREFIX,
                intent.certification.tree,
                intent.certification.gate,
                intent.pull_request
            );
            if let Some(incident) = tx.verification_incident(intent.project)?
                && incident.project == intent.project
                && incident.generation == intent.generation
            {
                tx.clear_verification_incident(&incident.incident_id)?;
            }
            tx.remove_landing_intent(intent)?;
            super::story::append_state_transition(
                tx,
                intent.project,
                intent.story,
                &row,
                &prefix,
                &states,
                &done,
                &now,
                vec![
                    StoryEvent::StoryCommentAdded {
                        at: now.clone(),
                        text: comment,
                    },
                    StoryEvent::StoryPrMerged {
                        at: now.clone(),
                        url: intent.pull_request.clone(),
                    },
                ],
                ctx.provenance(),
            )?;
            Ok(true)
        })?)
    }

    /// Releases an attempt only after the process proves no merge request was sent.
    pub(crate) fn release_unattempted_landing(
        &self,
        intent: &LandingIntent,
    ) -> Result<bool, AppError> {
        Ok(self.store.write(|tx| tx.remove_landing_intent(intent))?)
    }
}
