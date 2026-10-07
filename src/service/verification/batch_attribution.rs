//! A red prefix identifies a suspect; it does not confer repair authority.
use super::*;
use crate::service::attribution::{AttributionRecord, FailureCause, FailureComponent};
use crate::store::{BisectionOutcome, GateInputs, GateSubmission, VerificationBatch};

impl<S: Store> VerificationQueue<'_, S> {
    /// Bind a non-head suspect hold to the exact batch, live head admission and member generation.
    pub(crate) fn record_batch_suspect(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        head: &VerificationCandidate,
        attempt_id: &str,
        expected: &VerificationBatch,
    ) -> Result<GenerationWrite<()>, AppError> {
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let project = candidate.project;
            if ctx.project() != project || head.project != project || expected.project != project {
                return Err(StoreError::Validation("batch suspect crosses project authority".into()));
            }
            let prefix = project_prefix(tx, project)?;
            let (story, row) = resolve_story(tx, project, &prefix, &candidate.story_id)?;
            let (_, head_row) = resolve_story(tx, project, &prefix, &head.story_id)?;
            let batches = tx.verification_batches(project)?;
            let attempts = tx.gate_attempts(project)?;
            let current_attempt = attempts.iter().rev().find(|a| a.submission.matches_story(project, &head.story_id));
            if !candidate_is_current(tx, &row, candidate)? || !candidate_is_current(tx, &head_row, head)?
                || row.awaiting.is_some() || candidate.landing_pending || !tx.verification_enabled(project)?
                || tx.landing_intents()?.iter().any(|i| i.project == project && i.story == story)
                || !batches.iter().any(|b| b == expected && !b.retired && b.head == head.story_id)
                || current_attempt.is_none_or(|a| a.id != attempt_id || a.finished_at.is_some()
                    || a.submission.generation != head.verifying_generation) {
                return Ok(GenerationWrite::Superseded);
            }
            let attempt = current_attempt.expect("checked head admission");
            if attempt.control_revision != Some(tx.verification_control_revision(project)?) {
                return Ok(GenerationWrite::Superseded);
            }
            let Some(member) = expected.members.iter().find(|m| m.story == story && Some(m.generation) == candidate.verifying_generation
                && candidate.pull_request.as_ref().is_ok_and(|p| p.url == m.pull_request)) else { return Ok(GenerationWrite::Superseded); };
            let Some(BisectionOutcome::Culprit { story_id, tree, log, position, certified, .. }) = expected.bisection.as_ref().and_then(|b| b.outcome.as_ref()) else {
                return Err(StoreError::Validation("batch has no retained suspect observation".into()));
            };
            if story_id != &candidate.story_id || *position != member.position + 1 || member.merge_tree.as_ref() != Some(tree) {
                return Err(StoreError::Validation("batch suspect does not match the retained member".into()));
            }
            let submission = GateSubmission { project, story_id: candidate.story_id.clone(), generation: candidate.verifying_generation, submitted_at: candidate.verifying_since.clone() };
            if tx.attributions(project)?.iter().any(|a| a.attempt == attempt_id && a.submission.same_generation(&submission)) {
                return Ok(GenerationWrite::Applied(()));
            }
            let mut detail = format!("Batch {} retained a red prefix ending at {} (position {position}); {certified} preceding members were certified. Red tree {tree}; original log {log}; owner admission {attempt_id}. Bisection is localization, not a detector-preserving causal contrast. No repair is assigned.", expected.id, candidate.story_id);
            if let Some(resolution) = &member.resolution {
                detail.push_str(&format!(" Automated integration resolution: {}.", serde_json::to_string(resolution).map_err(|e| StoreError::Validation(format!("encoding retained resolution: {e}")))?));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let record = AttributionRecord { version: 1, id: id.clone(), revision: 0, submission, attempt: attempt_id.into(),
                inputs: GateInputs { head: Some(member.head_commit.clone()), base: Some(expected.base_commit.clone()), tree: Some(tree.clone()), ..Default::default() }, created_at: now.clone(),
                components: vec![FailureComponent { id: "batch-suspect".into(), check: "batch suspect".into(), signature: detail.clone(),
                    requirement: "Establish the exact detector and causal responsibility before assigning repair".into(), log: log.clone(),
                    observed_cause: if member.resolution.is_some() { FailureCause::Integration } else { FailureCause::Unknown } }],
                preparation: None, settlement: None, plans: vec![], probes: vec![], assessments: vec![], diagnosis_ms: 0, held: true, retired: None };
            tx.insert_attribution(&record)?;
            append_and_fold(tx, project, story, &prefix, &tx.state_map(project)?, ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryCommentAdded { at: now.clone(), text: format!("CENTRAL VERIFICATION ATTRIBUTION HELD — evidence {id}. {detail} Inspect `story verifier evidence {} --json`.", candidate.story_id) }], ctx.provenance())?;
            Ok(GenerationWrite::Applied(()))
        })?)
    }
}
