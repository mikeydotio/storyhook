//! Recovery effects cannot inherit authority from a newer story or operator action.

use super::{AssessmentHold, RecoveryView};
use crate::domain::{LABEL_HUMAN_ONLY, LABEL_NO_AUTO, StoryEvent, StorySnapshot};
use crate::store::{GlobalSeq, ProjectId, ReadOps, StoreError, StoryNo};

pub(super) fn state_revision(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<GlobalSeq, StoreError> {
    tx.events_for(project, story)?
        .iter()
        .rev()
        .find_map(|event| {
            matches!(event.known(), Some(StoryEvent::StoryStateChanged { .. }))
                .then_some(event.global_seq)
        })
        .ok_or_else(|| StoreError::Corrupt("recovery subject has no state transition".into()))
}

pub(super) fn label_revision(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<GlobalSeq>, StoreError> {
    Ok(tx
        .events_for(project, story)?
        .iter()
        .rev()
        .find_map(|event| {
            matches!(event.known(), Some(StoryEvent::StoryLabelsSet { labels, .. })
            if labels.iter().any(|label| label == LABEL_HUMAN_ONLY || label == LABEL_NO_AUTO))
            .then_some(event.global_seq)
        }))
}

pub(super) fn policy_hold(
    tx: &impl ReadOps,
    project: ProjectId,
    snapshot: &StorySnapshot,
) -> Result<Option<AssessmentHold>, StoreError> {
    if !tx.verification_enabled(project)? {
        return Ok(Some(AssessmentHold::OperatorStop));
    }
    if snapshot
        .labels
        .iter()
        .any(|label| label == LABEL_HUMAN_ONLY || label == LABEL_NO_AUTO)
    {
        return Ok(Some(AssessmentHold::ReservedLabel));
    }
    Ok(None)
}

pub(super) fn assessment_hold(
    tx: &impl ReadOps,
    view: &RecoveryView,
) -> Result<Option<AssessmentHold>, StoreError> {
    let project = view.record.project;
    let assessment = &view.state.assessment;
    let subject = view
        .state
        .subjects
        .iter()
        .find(|subject| {
            subject.story == assessment.story
                && subject.candidate.verifying_generation == Some(assessment.generation)
        })
        .ok_or_else(|| StoreError::Corrupt("assessment has no originating submission".into()))?;
    let Some(row) = tx.story(project, subject.story)? else {
        return Ok(Some(AssessmentHold::SubjectMissing));
    };
    if let Some(reason) = policy_hold(tx, project, &row.snapshot)? {
        return Ok(Some(reason));
    }
    if !subject.returned
        || row.state != super::super::verification::RETURNED_STATE
        || row.awaiting.is_some()
        || state_revision(tx, project, subject.story)? != subject.state_revision
        || label_revision(tx, project, subject.story)? != subject.label_revision
        || super::super::verification::verifying_entry(tx, project, subject.story)?
            .map(|(_, generation)| generation)
            != Some(assessment.generation)
    {
        return Ok(Some(AssessmentHold::AuthorityChanged));
    }
    let stories = super::super::query::story_map(tx, project)?;
    if crate::domain::is_blocked(&row.snapshot, &stories)
        || tx.story_resets(project)?.contains_key(&subject.story)
        || tx
            .story_reset(project, subject.story)?
            .is_some_and(|reset| !reset.completed)
        || tx.engine_reset(project, subject.story)?.is_some()
        || tx
            .landing_intents()?
            .iter()
            .any(|intent| intent.project == project && intent.story == subject.story)
    {
        return Ok(Some(AssessmentHold::ResourceOrDependency));
    }
    let blocked = tx
        .block_deliveries(project)?
        .iter()
        .rev()
        .find(|delivery| {
            delivery.story == subject.story
                && delivery.action == crate::store::BlockAction::Interrupt
        })
        .map(|delivery| delivery.id);
    if blocked != subject.candidate.blocking_revision {
        return Ok(Some(AssessmentHold::AuthorityChanged));
    }
    Ok(None)
}
