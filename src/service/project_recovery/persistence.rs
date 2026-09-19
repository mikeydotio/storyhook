//! Strict decoding and transactional revision changes for recovery state.

use super::{AssessmentStatus, FaultObservation, RecoveryState, RecoveryView};
use crate::store::{ProjectId, ProjectRecovery, ReadOps, StoreError, WriteOps};

pub(super) fn serialize(value: &impl serde::Serialize) -> Result<serde_json::Value, StoreError> {
    serde_json::to_value(value)
        .map_err(|error| StoreError::Validation(format!("serializing project recovery: {error}")))
}

pub(super) fn timestamp(value: &str) -> Result<chrono::DateTime<chrono::FixedOffset>, StoreError> {
    chrono::DateTime::parse_from_rfc3339(value).map_err(|error| {
        StoreError::Corrupt(format!("project recovery timestamp {value}: {error}"))
    })
}

pub(super) fn find(
    tx: &impl ReadOps,
    project: ProjectId,
    id: &str,
) -> Result<RecoveryView, StoreError> {
    let record = tx
        .project_recoveries(project)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| {
            StoreError::Validation(format!(
                "project recovery {id} does not exist in this project"
            ))
        })?;
    read_view(tx, record)
}

pub(super) fn read_view(
    tx: &impl ReadOps,
    record: ProjectRecovery,
) -> Result<RecoveryView, StoreError> {
    let state: RecoveryState = serde_json::from_value(record.state.clone()).map_err(|error| {
        StoreError::Corrupt(format!("project recovery {} state: {error}", record.id))
    })?;
    if state.version != 1 {
        return Err(StoreError::Corrupt(format!(
            "unsupported project recovery state version {}",
            state.version
        )));
    }
    let observations = tx.project_recovery_observations(record.project, &record.id)?;
    if state
        .subjects
        .iter()
        .any(|subject| subject.candidate.project != record.project)
        || !state.subjects.iter().any(|subject| {
            subject.story == state.assessment.story
                && subject.candidate.verifying_generation == Some(state.assessment.generation)
        })
        || (state.assessment.status == AssessmentStatus::Held) != state.assessment.hold.is_some()
        || state.assessment.failures > 3
        || (state.assessment.status == AssessmentStatus::Decided) != state.decision.is_some()
    {
        return Err(StoreError::Corrupt(format!(
            "project recovery {} has inconsistent assessment authority",
            record.id
        )));
    }
    if let Some(decision) = &state.decision {
        decision.input.validate().map_err(|error| {
            StoreError::Corrupt(format!("invalid retained recovery decision: {error}"))
        })?;
        let repair_consistent = match decision.input.scope {
            super::RepairScope::SameStory => decision.repair_story == Some(state.assessment.story),
            super::RepairScope::SeparateStory => decision
                .repair_story
                .is_some_and(|story| story != state.assessment.story),
            super::RepairScope::External => {
                decision.repair_story.is_none() && decision.owned_edges.is_empty()
            }
        };
        if decision.input.project != record.project
            || decision.input.generation != state.assessment.generation
            || decision.input.dispatch_identity != state.assessment.dispatch_identity
            || decision.input.revision >= record.revision
            || !repair_consistent
            || decision.repair_story.is_some() != decision.delivery_identity.is_some()
            || decision
                .owned_edges
                .iter()
                .any(|story| Some(*story) == decision.repair_story)
        {
            return Err(StoreError::Corrupt(
                "retained recovery decision has inconsistent authority or repair ownership".into(),
            ));
        }
    }
    for observation in &observations {
        let evidence: FaultObservation = serde_json::from_value(observation.evidence.clone())
            .map_err(|error| {
                StoreError::Corrupt(format!(
                    "project recovery observation {}: {error}",
                    observation.attempt_id
                ))
            })?;
        if evidence.version != 1
            || evidence.candidate.project != record.project
            || evidence.candidate.verifying_generation != Some(observation.generation)
        {
            return Err(StoreError::Corrupt(format!(
                "project recovery observation {} has inconsistent identity",
                observation.attempt_id
            )));
        }
        evidence.fault.validate().map_err(|error| {
            StoreError::Corrupt(format!(
                "project recovery observation {}: {error}",
                observation.attempt_id
            ))
        })?;
    }
    Ok(RecoveryView {
        record,
        state,
        observations,
    })
}

pub(super) fn save(
    tx: &mut impl WriteOps,
    view: &mut RecoveryView,
    now: &str,
) -> Result<(), StoreError> {
    let expected = view.record.revision;
    view.record.revision = expected
        .checked_add(1)
        .ok_or_else(|| StoreError::Corrupt("project recovery revision overflow".into()))?;
    view.state.updated_at = now.into();
    view.record.state = serialize(&view.state)?;
    if !tx.update_project_recovery(&view.record, expected)? {
        return Err(StoreError::Validation(
            "project recovery changed before its update".into(),
        ));
    }
    Ok(())
}
