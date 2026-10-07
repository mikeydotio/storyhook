//! Current holds are a projection of evidence and story generation, never a second queue.
use super::{FailureCause, classify};
use crate::store::{GlobalSeq, ProjectId, ReadOps, StoreError, StoryQuery};
use serde::{Deserialize, Serialize};

/// One currently held failure component, shared by CLI and dashboard status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionHold {
    /// Submitted story identity.
    pub story_id: String,
    /// Exact verifying generation.
    pub generation: Option<GlobalSeq>,
    /// Retained attribution record to inspect.
    pub evidence_id: String,
    /// Independently observed failure component.
    pub component: String,
    /// Responsibility supported by retained observations; never a return capability.
    pub cause: FailureCause,
    /// Current diagnosis state, distinct from gate progress.
    pub diagnosis: String,
    /// Next action required before this work may progress.
    pub next_action: String,
}

/// Whether any retained observation holds this exact generation.
pub(crate) fn held(
    tx: &impl ReadOps,
    project: ProjectId,
    story: &str,
    generation: Option<GlobalSeq>,
) -> Result<bool, StoreError> {
    Ok(tx.attributions(project)?.iter().any(|a| {
        a.held
            && a.retired.is_none()
            && a.submission.matches_story(project, story)
            && a.submission.generation == generation
    }))
}

/// Read every current hold from one store snapshot; old generations remain history only.
pub(crate) fn current(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<Vec<AttributionHold>, StoreError> {
    let Some(project) = tx.project(project)? else {
        return Ok(vec![]);
    };
    let rows = tx.stories(
        project.id,
        &StoryQuery::all().state(super::super::verification::VERIFYING_STATE),
    )?;
    let mut result = Vec::new();
    let records = tx.attributions(project.id)?;
    for record in records.iter().filter(|a| a.held && a.retired.is_none()) {
        let story = record.submission.story_number().ok_or_else(|| {
            StoreError::Corrupt(format!(
                "attribution {} has invalid recorded story identity {}",
                record.id, record.submission.story_id
            ))
        })?;
        if !rows.iter().any(|r| r.story_no == story)
            || super::super::verification::verifying_entry(tx, project.id, story)?
                .map(|(_, seq)| seq)
                != record.submission.generation
        {
            continue;
        }
        // Retiring an attempt does not refund this submission's diagnosis allowance.
        let mut milliseconds = 0u64;
        let mut starts = 0usize;
        let mut unsettled = false;
        for attempt in records
            .iter()
            .filter(|a| a.submission.same_generation(&record.submission))
        {
            milliseconds = milliseconds.saturating_add(attempt.diagnosis_ms);
            starts = starts.saturating_add(attempt.probes.len());
            unsettled |= attempt.has_unsettled_diagnosis();
        }
        let diagnosis = if unsettled {
            "unsettled execution"
        } else if milliseconds >= super::MAX_DIAGNOSIS_MS || starts >= super::MAX_PROBES {
            "diagnosis allowance exhausted"
        } else {
            "causal evidence required"
        };
        let story_id = story.to_id(&project.prefix);
        for component in &record.components {
            result.push(AttributionHold {
                story_id: story_id.clone(), generation: record.submission.generation,
                evidence_id: record.id.clone(), component: component.id.clone(), cause: classify(record, component),
                diagnosis: diagnosis.into(), next_action: format!("Inspect story verifier evidence {story_id} --json; establish cause before retry or repair"),
            });
        }
    }
    Ok(result)
}
