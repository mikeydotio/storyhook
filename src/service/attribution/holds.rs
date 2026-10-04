//! Current holds are a projection of evidence and story generation, never a second queue.
use super::{FailureCause, classify};
use crate::store::{GlobalSeq, ProjectId, ReadOps, StoreError, StoryNo, StoryQuery};
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
            && a.submission.story_id == story
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
        let story = StoryNo::parse_id(&project.prefix, &record.submission.story_id)?;
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
        for attempt in records.iter().filter(|a| {
            a.submission.story_id == record.submission.story_id
                && a.submission.generation == record.submission.generation
        }) {
            milliseconds = milliseconds.saturating_add(attempt.diagnosis_ms);
            starts = starts.saturating_add(attempt.probes.len());
            unsettled |= attempt.probes.iter().any(|p| p.completed.is_none());
        }
        let diagnosis = if unsettled {
            "unsettled execution"
        } else if milliseconds >= super::MAX_DIAGNOSIS_MS || starts >= super::MAX_PROBES {
            "diagnosis allowance exhausted"
        } else {
            "causal evidence required"
        };
        for component in &record.components {
            result.push(AttributionHold {
                story_id: record.submission.story_id.clone(), generation: record.submission.generation,
                evidence_id: record.id.clone(), component: component.id.clone(), cause: classify(record, component),
                diagnosis: diagnosis.into(), next_action: format!("Inspect story verifier evidence {} --json; establish cause before retry or repair", record.submission.story_id),
            });
        }
    }
    Ok(result)
}
