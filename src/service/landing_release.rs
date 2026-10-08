//! Audited release of definitive refusals and explicit operator decisions.
use super::{Ctx, VerificationQueue};
use crate::error::AppError;
use crate::store::{BatchPhase, ExpectedSeq, LandingIntent, ReadOps, Store, StoreError, WriteOps};

impl<S: Store> VerificationQueue<'_, S> {
    /// Releases a definitive refused request, retaining its causal evidence on
    /// every member. This grants neither completion nor gate certification.
    pub(crate) fn release_rejected_landing(
        &self,
        ctx: &Ctx<'_, S>,
        intent: &LandingIntent,
        detail: &str,
    ) -> Result<bool, AppError> {
        self.release_landing_record(
            ctx,
            intent,
            &format!(
                "CENTRAL LANDING REFUSED\n\n{}",
                crate::text_lint::quote_evidence(detail)
            ),
            None,
        )
    }

    /// Releases a pending single or batch intent after a fresh remote read.
    /// The supplied observer is read-only; the CLI holds the runtime admission
    /// reservation around this whole call. There is deliberately no force flag.
    pub fn release_landing_observed(
        &self,
        ctx: &Ctx<'_, S>,
        id: &str,
        reason: &str,
        observe: impl FnOnce(&LandingIntent) -> Result<(String, String), AppError>,
    ) -> Result<Vec<String>, AppError> {
        if ctx.is_agent_session() {
            return Err(AppError::Validation(
                "landing release requires an operator, not a dispatched agent session".into(),
            ));
        }
        if reason.trim().is_empty() {
            return Err(AppError::Validation(
                "landing release requires a nonempty reason".into(),
            ));
        }
        let rows = self.store.read(|tx| tx.landing_intents())?;
        let intent = rows
            .iter()
            .find(|row| row.project == ctx.project() && row.id == id)
            .ok_or_else(|| {
                AppError::Validation("pending landing intent not found in this project".into())
            })?;
        let members: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.project == intent.project
                    && (row.id == intent.id || intent.batch.is_some() && row.batch == intent.batch)
            })
            .map(|row| row.story_id.clone())
            .collect();
        let (state, head) = observe(intent)?;
        if !matches!(state.as_str(), "OPEN" | "CLOSED") || head != intent.certification.head() {
            return Err(AppError::Validation(format!(
                "landing release refused: remote state {state} or head differs; MERGED and unknown outcomes require reconciliation"
            )));
        }
        let detail = format!(
            "OPERATOR LANDING RELEASE — intent {}; PR {}; admitted head {}; observed {state}. An earlier remote request may still complete; this release is not proof of non-merge or certification.\n\nReason:\n\n{}",
            intent.id,
            intent.landing_pull_request(),
            head,
            crate::text_lint::quote_evidence(reason.trim())
        );
        if !self.release_landing_record(ctx, intent, &detail, Some(&rows))? {
            return Err(AppError::Validation(
                "landing intent changed during remote observation; nothing released".into(),
            ));
        }
        Ok(members)
    }

    fn release_landing_record(
        &self,
        ctx: &Ctx<'_, S>,
        intent: &LandingIntent,
        detail: &str,
        observed: Option<&[LandingIntent]>,
    ) -> Result<bool, AppError> {
        if ctx.project() != intent.project {
            return Err(AppError::Validation(
                "landing context belongs to another project".into(),
            ));
        }
        Ok(self.store.write(|tx| {
            let pending = tx.landing_intents()?;
            if !pending.contains(intent) {
                return Ok(false);
            }
            let rows: Vec<_> = pending
                .into_iter()
                .filter(|row| {
                    row.project == intent.project
                        && (row.id == intent.id
                            || intent.batch.is_some() && row.batch == intent.batch)
                })
                .collect();
            if let Some(observed) = observed {
                let before: Vec<_> = observed
                    .iter()
                    .filter(|row| {
                        row.project == intent.project
                            && (row.id == intent.id
                                || intent.batch.is_some() && row.batch == intent.batch)
                    })
                    .collect();
                if before.len() != rows.len() || rows.iter().any(|row| !before.contains(&row)) {
                    return Ok(false);
                }
            }
            if let Some(binding) = &intent.batch {
                let record = tx
                    .verification_batches(intent.project)?
                    .into_iter()
                    .find(|record| record.id == binding.id)
                    .ok_or_else(|| StoreError::NotFound(format!("batch {}", binding.id)))?;
                if record.phase != BatchPhase::Landing {
                    return Err(StoreError::Validation(
                        "a landed or changed batch cannot be released".into(),
                    ));
                }
                let mut next = record.advance(BatchPhase::Released, &ctx.now())?;
                next.detail = Some(detail.to_owned());
                if !tx.update_verification_batch(&next, record.revision)? {
                    return Err(StoreError::Invariant(
                        "batch changed during landing release".into(),
                    ));
                }
            }
            for row in &rows {
                super::landing::release_intent(tx, row)?;
            }
            let prefix = super::project_prefix(tx, intent.project)?;
            let states = tx.state_map(intent.project)?;
            for intent in rows {
                let row = tx
                    .story(intent.project, intent.story)?
                    .ok_or_else(|| StoreError::NotFound(intent.story_id.clone()))?;
                super::append_and_fold(
                    tx,
                    intent.project,
                    intent.story,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(row.head_seq),
                    &[crate::domain::StoryEvent::StoryCommentAdded {
                        at: ctx.now(),
                        text: format!(
                            "{detail}\n\nReleased member intent {} for {} at generation {}.",
                            intent.id, intent.story_id, intent.generation
                        ),
                    }],
                    ctx.provenance(),
                )?;
            }
            Ok(true)
        })?)
    }
}
