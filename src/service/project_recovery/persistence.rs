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
    {
        return Err(StoreError::Corrupt(format!(
            "project recovery {} has inconsistent assessment authority",
            record.id
        )));
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
