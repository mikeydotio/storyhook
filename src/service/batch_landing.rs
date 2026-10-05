//! Admission, completion and release of a verification batch's landing
//! (SH-832; spec positions B6 and B10 in `docs/spec/verification-batching.md`).
//!
//! A batch lands through ordinary per-story landing intents, one per member,
//! bound by a [`BatchLanding`]: every guard a single landing has holds for
//! each member unchanged, and `landing::validate_pending` ties the rows to
//! each other and to the batch record before every commit. Each operation
//! here is one transaction over every member.

use super::block_delivery::{SubmissionGate, derive_block_edges};
use super::landing::{
    Admissible, admissible, admit_intent, completable, complete_story, pending_intent,
    release_intent,
};
use super::{Ctx, VerificationCandidate, VerificationQueue};
use crate::domain::landing::VerifiedSubmission;
use crate::error::AppError;
use crate::store::{
    BatchGate, BatchLanding, BatchLandingIntent, BatchPhase, LandingIntent, ReadOps, Store,
    StoreError, StoryNo, VerificationBatch, WriteOps,
};

/// Whether a certified batch may begin its merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchLandingAdmission {
    /// Every member now holds a landing intent and the record is `landing`.
    Admitted {
        /// The member intents.
        intent: Box<BatchLandingIntent>,
        /// The batch record as written.
        record: Box<VerificationBatch>,
    },
    /// The batch may not land; nothing was written. The detail says why.
    Refused(String),
}

impl<S: Store> VerificationQueue<'_, S> {
    /// Atomically admits a certified batch to land: re-derives every member
    /// exactly as a single landing does, writes one landing intent per
    /// member bound to the batch, and moves the record from `gating` to
    /// `landing` with its `gate`. `members` are the members' candidates in
    /// batch order.
    pub fn begin_batch_landing(
        &self,
        ctx: &Ctx<'_, S>,
        record: &VerificationBatch,
        members: &[VerificationCandidate],
        certification: &VerifiedSubmission,
        gate: BatchGate,
    ) -> Result<BatchLandingAdmission, AppError> {
        certification.validate()?;
        if ctx.project() != record.project {
            return Err(AppError::Validation(
                "landing context belongs to another project".into(),
            ));
        }
        if certification.head != record.tip {
            return Ok(BatchLandingAdmission::Refused(format!(
                "the gate certified head {}, not the batch tip {}",
                certification.head, record.tip
            )));
        }
        let Some(pull_request) = record.pull_request.clone() else {
            return Ok(BatchLandingAdmission::Refused(
                "the batch has no pull request".into(),
            ));
        };
        if members.len() != record.members.len()
            || members
                .iter()
                .zip(&record.members)
                .any(|(candidate, member)| candidate.story_id != member.story_id)
        {
            return Ok(BatchLandingAdmission::Refused(
                "the candidates are not the batch's members".into(),
            ));
        }
        let now = ctx.now();
        Ok(self.store.write(|tx| {
            let Some(current) = tx
                .verification_batches(record.project)?
                .into_iter()
                .find(|known| known.id == record.id)
            else {
                return Ok(BatchLandingAdmission::Refused(
                    "the batch record is gone".into(),
                ));
            };
            if current != *record || current.phase != BatchPhase::Gating {
                return Ok(BatchLandingAdmission::Refused(
                    "the batch record changed since its gate".into(),
                ));
            }
            let batch = BatchLanding {
                id: record.id.clone(),
                landing: uuid::Uuid::new_v4().to_string(),
                pull_request: pull_request.url.clone(),
            };
            let prefix = super::project_prefix(tx, record.project)?;
            let mut rows = Vec::with_capacity(members.len());
            for (candidate, member) in members.iter().zip(&record.members) {
                if let Some(pending) = pending_intent(tx, candidate)? {
                    return Ok(BatchLandingAdmission::Refused(format!(
                        "{} already has pending landing {}",
                        candidate.story_id, pending.id
                    )));
                }
                let (generation, pull_request) = match admissible(tx, candidate)? {
                    Admissible::Ready {
                        generation,
                        pull_request,
                    } => (generation, pull_request),
                    Admissible::Held(blockers) => {
                        return Ok(BatchLandingAdmission::Refused(format!(
                            "{} is held by open blockers: {}",
                            candidate.story_id,
                            blockers.join(", ")
                        )));
                    }
                    Admissible::Superseded => {
                        return Ok(BatchLandingAdmission::Refused(format!(
                            "{} changed after the batch gate",
                            candidate.story_id
                        )));
                    }
                };
                if generation != member.generation || pull_request != member.pull_request {
                    return Ok(BatchLandingAdmission::Refused(format!(
                        "{} is not the generation or pull request the batch gated",
                        candidate.story_id
                    )));
                }
                rows.push(LandingIntent {
                    id: uuid::Uuid::new_v4().to_string(),
                    project: record.project,
                    story: StoryNo::parse_id(&prefix, &candidate.story_id)?,
                    story_id: candidate.story_id.clone(),
                    project_slug: candidate.project_slug.clone(),
                    generation,
                    pull_request,
                    checkout: candidate.checkout.clone(),
                    certification: certification.clone().into(),
                    created_at: now.clone(),
                    batch: Some(batch.clone()),
                });
            }
            for row in &rows {
                admit_intent(tx, row)?;
            }
            let mut next = record.advance(BatchPhase::Landing, &now)?;
            next.gate = Some(gate);
            if !tx.update_verification_batch(&next, record.revision)? {
                return Err(StoreError::Invariant(format!(
                    "verification batch {} changed while its landing was admitted",
                    record.id
                )));
            }
            rows.sort_by_key(|row| row.story);
            Ok(BatchLandingAdmission::Admitted {
                intent: Box::new(BatchLandingIntent {
                    batch,
                    certification: certification.clone().into(),
                    rows,
                }),
                record: Box::new(next),
            })
        })?)
    }

    /// Records a confirmed batch merge: in one transaction, completes every
    /// member row the verifier may complete (its candidate is among
    /// `members` and still human-permitted) and moves a `landing` record to
    /// `landed`. A member a person holds keeps its row, exactly as a single
    /// human-only landing does. Answers the stories completed; a fault
    /// rolls every one of them back.
    pub fn complete_batch_landing(
        &self,
        ctx: &Ctx<'_, S>,
        intent: &BatchLandingIntent,
        detail: &str,
        members: &[VerificationCandidate],
    ) -> Result<Vec<String>, AppError> {
        let project = intent
            .rows
            .first()
            .map(|row| row.project)
            .ok_or_else(|| AppError::Validation("a batch landing has no members".into()))?;
        if ctx.project() != project {
            return Err(AppError::Validation(
                "landing context belongs to another project".into(),
            ));
        }
        // Completion is a story mutation like any other: the stories it
        // unblocks are owed their Resume (SH-772).
        Ok(self.store.write(|tx| {
            derive_block_edges(tx, project, SubmissionGate::NotASubmission, |tx| {
                let Some(record) = tx
                    .verification_batches(project)?
                    .into_iter()
                    .find(|record| record.id == intent.batch.id)
                else {
                    return Ok(Vec::new());
                };
                let rows: Vec<LandingIntent> = tx
                    .landing_intents()?
                    .into_iter()
                    .filter(|row| row.batch.as_ref() == Some(&intent.batch))
                    .collect();
                let mut completed = Vec::new();
                for row in &rows {
                    let Some(candidate) = members
                        .iter()
                        .find(|candidate| candidate.story_id == row.story_id)
                    else {
                        continue;
                    };
                    if !completable(tx, row, Some(candidate))? {
                        continue;
                    }
                    complete_story(tx, ctx, row, green_comment(&record, row, detail))?;
                    completed.push(row.story_id.clone());
                }
                if !completed.is_empty() && record.phase == BatchPhase::Landing {
                    let mut next = record.advance(BatchPhase::Landed, &ctx.now())?;
                    next.detail = Some(format!(
                        "landed; {} done in one transaction",
                        completed.join(", ")
                    ));
                    if !tx.update_verification_batch(&next, record.revision)? {
                        return Err(StoreError::Invariant(format!(
                            "verification batch {} changed while its members completed",
                            record.id
                        )));
                    }
                }
                Ok(completed)
            })
        })?)
    }

    /// Releases a batch landing whose merge request was provably never sent:
    /// every member row is removed and the record moves from `landing` to
    /// `released` with `detail`, in one transaction. Answers the record as
    /// written.
    pub fn release_unattempted_batch_landing(
        &self,
        ctx: &Ctx<'_, S>,
        intent: &BatchLandingIntent,
        detail: &str,
    ) -> Result<VerificationBatch, AppError> {
        let project = ctx.project();
        Ok(self.store.write(|tx| {
            let record = tx
                .verification_batches(project)?
                .into_iter()
                .find(|record| record.id == intent.batch.id)
                .ok_or_else(|| {
                    StoreError::NotFound(format!("verification batch {}", intent.batch.id))
                })?;
            for row in tx
                .landing_intents()?
                .into_iter()
                .filter(|row| row.batch.as_ref() == Some(&intent.batch))
            {
                release_intent(tx, &row)?;
            }
            let mut next = record.advance(BatchPhase::Released, &ctx.now())?;
            next.detail = Some(detail.to_owned());
            if !tx.update_verification_batch(&next, record.revision)? {
                return Err(StoreError::Invariant(format!(
                    "verification batch {} changed while its landing was released",
                    record.id
                )));
            }
            Ok(next)
        })?)
    }
}

/// A member's GREEN: the batch, its pull request, the other members and the
/// member's own pull request, then the landing evidence.
fn green_comment(record: &VerificationBatch, row: &LandingIntent, detail: &str) -> String {
    let others: Vec<&str> = record
        .members
        .iter()
        .filter(|member| member.story != row.story)
        .map(|member| member.story_id.as_str())
        .collect();
    format!(
        "{} merge tree `{}` passed `{}` in verification batch {} with {}, and batch pull request {} landed; this story's pull request {} merged with it.{}\n\n{}",
        super::VERIFICATION_GREEN_PREFIX,
        row.certification.tree(),
        row.certification
            .certified()
            .expect("validated certified batch")
            .gate,
        record.id,
        others.join(", "),
        row.landing_pull_request(),
        row.pull_request,
        resolution_note(record, row.story).unwrap_or_default(),
        crate::text_lint::quote_evidence(detail)
    )
}

/// What a member's GREEN adds when the batch's last merge carries an
/// automated resolution this member took part in: the smoothed member, or
/// one it conflicted with (SH-834, council decision D1 (c)). Two members'
/// additions now sit side by side in files no test may read, so each of
/// them is told where to look.
fn resolution_note(record: &VerificationBatch, story: StoryNo) -> Option<String> {
    let last = record.members.last()?;
    let resolution = last.resolution.as_ref()?;
    let involved = last.story == story
        || record.members.iter().any(|member| {
            member.story == story && resolution.conflicted_with.contains(&member.story_id)
        });
    if !involved {
        return None;
    }
    let files: Vec<String> = resolution
        .files
        .iter()
        .map(|file| format!("`{}`", file.path))
        .collect();
    let mut members = resolution.conflicted_with.clone();
    members.push(last.story_id.clone());
    Some(format!(
        " The batch merge commit {} carries an automated conflict resolution ({}) of {} between {}: it keeps both additions, the earlier member's first, and no model wrote it. Check that the two additions agree.",
        last.merge_commit.as_deref().unwrap_or("of the last member"),
        resolution.strategy,
        files.join(", "),
        members.join(" and ")
    ))
}
