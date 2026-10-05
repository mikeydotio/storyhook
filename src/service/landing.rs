//! Admission and resolution of durable verifier landing authority.

use super::block_delivery::{SubmissionGate, derive_block_edges};
use super::{Ctx, VerificationCandidate, VerificationQueue};
use crate::domain::landing::LandingAuthority;
pub use crate::domain::landing::VerifiedSubmission;
use crate::error::AppError;
use crate::store::{GlobalSeq, LandingIntent, ReadOps, Store, StoreError, StoryNo, WriteOps};

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
        self.begin_authorized_landing(ctx, candidate, &certification.clone().into())
    }

    /// Admits evidence supplied by the owned attempt, with the same submission fences.
    pub(crate) fn begin_authorized_landing(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        certification: &LandingAuthority,
    ) -> Result<LandingAdmission, AppError> {
        certification.validate()?;
        if ctx.project() != candidate.project {
            return Err(AppError::Validation(
                "landing context belongs to another project".into(),
            ));
        }
        let current = self.current_for(candidate)?;
        if current.as_ref().is_none_or(|current| {
            current.pull_request != candidate.pull_request
                || current.pull_request.is_err()
                || current.checkout != candidate.checkout
        }) {
            return Ok(LandingAdmission::Superseded);
        }
        Ok(self.store.write(|tx| {
            if let Some(intent) = pending_intent(tx, candidate)? {
                return Ok(LandingAdmission::Pending(intent));
            }
            let (generation, pull_request) = match admissible(tx, candidate)? {
                Admissible::Ready {
                    generation,
                    pull_request,
                } => (generation, pull_request),
                Admissible::Held(blockers) => return Ok(LandingAdmission::Held(blockers)),
                Admissible::Superseded => return Ok(LandingAdmission::Superseded),
            };
            let prefix = super::project_prefix(tx, candidate.project)?;
            let intent = LandingIntent {
                id: uuid::Uuid::new_v4().to_string(),
                project: candidate.project,
                story: StoryNo::parse_id(&prefix, &candidate.story_id)?,
                story_id: candidate.story_id.clone(),
                project_slug: candidate.project_slug.clone(),
                generation,
                pull_request,
                checkout: candidate.checkout.clone(),
                certification: certification.clone(),
                created_at: ctx.now(),
                batch: None,
            };
            admit_intent(tx, &intent)?;
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
        self.complete_landing_guarded(ctx, intent, detail, None)
    }

    /// Resolves a daemon attempt only if its human reservation is still current.
    pub(crate) fn complete_landing_for(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        intent: &LandingIntent,
        detail: &str,
    ) -> Result<bool, AppError> {
        if candidate.project != intent.project || candidate.story_id != intent.story_id {
            return Err(AppError::Validation(
                "landing attempt belongs to another story".into(),
            ));
        }
        self.complete_landing_guarded(ctx, intent, detail, Some(candidate))
    }

    fn complete_landing_guarded(
        &self,
        ctx: &Ctx<'_, S>,
        intent: &LandingIntent,
        detail: &str,
        candidate: Option<&VerificationCandidate>,
    ) -> Result<bool, AppError> {
        if ctx.project() != intent.project {
            return Err(AppError::Validation(
                "landing context belongs to another project".into(),
            ));
        }
        // Completion is a story mutation like any other: it closes this story
        // and retracts the edges it imposed, so the stories it unblocks are
        // owed their Resume (SH-772). It moves nothing into `verifying`.
        Ok(self.store.write(|tx| {
            derive_block_edges(tx, intent.project, SubmissionGate::NotASubmission, |tx| {
                if !completable(tx, intent, candidate)? {
                    return Ok(false);
                }
                let comment = match &intent.certification {
                    LandingAuthority::Certified(certified) => format!(
                        "{} merge tree `{}` passed `{}` and pull request {} landed.\n\n{}",
                        super::VERIFICATION_GREEN_PREFIX, certified.tree, certified.gate,
                        intent.pull_request, crate::text_lint::quote_evidence(detail)
                    ),
                    LandingAuthority::Skipped(prepared) => format!(
                        "{} pull request {} landed with head `{}` and merge tree `{}`. Verification was stopped at admission {}; no gate ran. Release gates retain test coverage.\n\n{}",
                        super::verification::VERIFICATION_SKIPPED_PREFIX, intent.pull_request,
                        prepared.head, prepared.tree, prepared.attempt,
                        crate::text_lint::quote_evidence(detail)
                    ),
                };
                complete_story(tx, ctx, intent, comment)?;
                Ok(true)
            })
        })?)
    }

    /// Releases an attempt only after the process proves no merge request was sent.
    pub(crate) fn release_unattempted_landing(
        &self,
        intent: &LandingIntent,
    ) -> Result<bool, AppError> {
        Ok(self.store.write(|tx| release_intent(tx, intent))?)
    }
}

/// The pending landing intent of `candidate`'s story, if one exists.
pub(super) fn pending_intent(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
) -> Result<Option<LandingIntent>, StoreError> {
    Ok(tx
        .landing_intents()?
        .into_iter()
        .find(|i| i.project == candidate.project && i.story_id == candidate.story_id))
}

/// Whether a candidate's submission may still be admitted to land, read
/// inside the admitting transaction.
pub(super) enum Admissible {
    /// The submission is current: its generation and linked pull request.
    Ready {
        /// The submitted generation.
        generation: GlobalSeq,
        /// The linked pull request's URL.
        pull_request: String,
    },
    /// Open dependencies hold the submission.
    Held(Vec<String>),
    /// The submission changed since the candidate was read.
    Superseded,
}

/// Re-derives `candidate` from the queue inside `tx` and compares every
/// field its landing authority depends on.
pub(super) fn admissible(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
) -> Result<Admissible, StoreError> {
    let Some(current) = super::verification::ordered_candidates_for(tx, candidate.project)?
        .into_iter()
        .find(|c| c.project == candidate.project && c.story_id == candidate.story_id)
    else {
        return Ok(Admissible::Superseded);
    };
    if current.verifying_generation != candidate.verifying_generation
        || current.human_only_revision != candidate.human_only_revision
        || current.pull_request != candidate.pull_request
        || current.checkout != candidate.checkout
        || current.project_slug != candidate.project_slug
    {
        return Ok(Admissible::Superseded);
    }
    if !current.blocked_by.is_empty() {
        return Ok(Admissible::Held(current.blocked_by));
    }
    if current.blocking_revision != candidate.blocking_revision {
        return Ok(Admissible::Superseded);
    }
    let Some(generation) = current.verifying_generation else {
        return Ok(Admissible::Superseded);
    };
    let pull_request = current
        .pull_request
        .map_err(|problem| StoreError::Validation(problem.message()))?
        .url;
    Ok(Admissible::Ready {
        generation,
        pull_request,
    })
}

/// Whether `intent` may be completed now: it is still pending unchanged, it
/// still validates, its story is not a person's, and (for a daemon attempt)
/// the attempt's human reservation is still current.
pub(super) fn completable(
    tx: &impl ReadOps,
    intent: &LandingIntent,
    candidate: Option<&VerificationCandidate>,
) -> Result<bool, StoreError> {
    if let Some(candidate) = candidate
        && !super::verification::human::permits(tx, candidate)?
    {
        return Ok(false);
    }
    if !tx.landing_intents()?.contains(intent) {
        return Ok(false);
    }
    crate::store::landing::validate_intent(tx, intent)?;
    let row = tx
        .story(intent.project, intent.story)?
        .ok_or_else(|| StoreError::NotFound(intent.story_id.clone()))?;
    // A remote merge may already have happened. Keep its intent for
    // reconciliation rather than completing a person's reserved work.
    Ok(!crate::domain::is_human_only(&row.snapshot))
}

/// Completes one story whose landing is confirmed: `comment` (a GREEN),
/// `StoryPrMerged` for the story's own pull request and the move to the
/// completion state, with its intent released, in the caller's transaction.
pub(super) fn complete_story<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    intent: &LandingIntent,
    comment: String,
) -> Result<(), StoreError> {
    use crate::domain::{StoryEvent, completion_state};
    let prefix = super::project_prefix(tx, intent.project)?;
    let row = tx
        .story(intent.project, intent.story)?
        .ok_or_else(|| StoreError::NotFound(intent.story_id.clone()))?;
    let done = completion_state(&tx.states(intent.project)?)
        .ok_or_else(|| StoreError::Validation("project lacks required done state".into()))?;
    let states = tx.state_map(intent.project)?;
    let now = ctx.now();
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
    super::project_recovery::record_landing(tx, intent, &now)?;
    Ok(())
}

/// Acquires `intent` as durable landing authority in the caller's
/// transaction, after validating it. This module is the one owner of every
/// landing-authority mutation (`tests/state_set_funnel.rs`); a batch admits
/// one intent per member through here (SH-832).
pub(super) fn admit_intent(
    tx: &mut impl WriteOps,
    intent: &LandingIntent,
) -> Result<(), StoreError> {
    crate::store::landing::validate_intent(tx, intent)?;
    tx.insert_landing_intent(intent)
}

/// Releases `intent` because a story reset supersedes it (SH-886, D4).
///
/// The merge outcome is unknown: the pull request may still merge, and the
/// verifier treats the vanished intent as not completable. Answers what was
/// superseded, as a person reads it.
pub(crate) fn supersede_for_reset(
    tx: &mut impl WriteOps,
    intent: &LandingIntent,
) -> Result<String, StoreError> {
    tx.remove_landing_intent(intent)?;
    Ok(format!(
        "the pending landing of {} (its merge outcome is unknown; the pull request can still merge)",
        intent.pull_request
    ))
}

/// Releases `intent` in the caller's transaction, for a merge that was
/// provably never requested.
pub(super) fn release_intent(
    tx: &mut impl WriteOps,
    intent: &LandingIntent,
) -> Result<bool, StoreError> {
    tx.remove_landing_intent(intent)
}
