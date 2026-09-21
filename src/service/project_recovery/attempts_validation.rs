//! Persisted admission is evidence only when it belongs to the accepted repair.

use super::{RecoveryState, persistence::timestamp};
use crate::store::StoreError;
use std::collections::BTreeSet;

pub(super) fn validate(
    state: &RecoveryState,
    project: crate::store::ProjectId,
) -> Result<(), StoreError> {
    let repair = state.decision.as_ref().and_then(|d| d.repair_story);
    let mut attempt_ids = BTreeSet::new();
    let mut completed = BTreeSet::new();
    for attempt in &state.attempts {
        attempt
            .input
            .validate()
            .map_err(|error| StoreError::Corrupt(format!("repair attempt input: {error}")))?;
        timestamp(&attempt.admitted_at)?;
        if let Some(at) = &attempt.completed_at {
            timestamp(at)?;
        }
        if Some(attempt.story) != repair
            || attempt.candidate.project != project
            || attempt.candidate.verifying_generation != Some(attempt.generation)
            || attempt.id.trim().is_empty()
            || !attempt_ids.insert(&attempt.id)
            || attempt.generation <= state.assessment.generation
            || attempt.completion.is_some() != attempt.completed_at.is_some()
            || attempt
                .judgment
                .as_ref()
                .map(super::RepairJudgment::classification)
                != attempt.completion
        {
            return Err(StoreError::Corrupt(
                "repair attempt has inconsistent identity, owner, or completion evidence".into(),
            ));
        }
        if let Some(judgment) = &attempt.judgment {
            judgment.validate_against(&attempt.input).map_err(|error| {
                StoreError::Corrupt(format!("retained repair judgment: {error}"))
            })?;
        }
        if attempt.completion.is_some() {
            completed.insert(&attempt.input.head_tree);
        }
    }
    if completed.len() > 3 {
        return Err(StoreError::Corrupt(
            "recovery exceeds three completed committed repair inputs".into(),
        ));
    }
    let mut refusal_ids = BTreeSet::new();
    for refusal in &state.refusals {
        refusal
            .input
            .validate()
            .map_err(|error| StoreError::Corrupt(format!("repair refusal input: {error}")))?;
        timestamp(&refusal.at)?;
        if Some(refusal.story) != repair
            || refusal.candidate.project != project
            || refusal.candidate.verifying_generation != Some(refusal.generation)
            || refusal.id.trim().is_empty()
            || !refusal_ids.insert(&refusal.id)
            || refusal.generation <= state.assessment.generation
        {
            return Err(StoreError::Corrupt(
                "repair refusal has inconsistent identity or owner".into(),
            ));
        }
        if let Some(attempt) = state
            .attempts
            .iter()
            .find(|attempt| attempt.id == refusal.id)
            && (attempt.story != refusal.story
                || attempt.generation != refusal.generation
                || attempt.input != refusal.input)
        {
            return Err(StoreError::Corrupt(
                "repair refusal conflicts with its earlier admitted input".into(),
            ));
        }
    }
    Ok(())
}
