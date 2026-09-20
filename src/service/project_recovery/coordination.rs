//! Durable recovery ownership prevents a competing engine hard-stop classification.
use super::{AssessmentStatus, WorkKind, WorkStatus, authority, persistence};
use crate::store::{ProjectId, ReadOps, StoreError, StoryNo};

/// Generations already judged as project faults cannot become fresh gate attempts.
pub(crate) fn observed_generations(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<std::collections::BTreeSet<(StoryNo, crate::store::GlobalSeq)>, StoreError> {
    let mut observed = std::collections::BTreeSet::new();
    for record in tx.project_recoveries(project)? {
        let view = persistence::read_view(tx, record)?;
        for observation in view.observations {
            observed.insert((observation.story, observation.generation));
        }
    }
    Ok(observed)
}

pub(crate) fn owns_coordination(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    now: &str,
) -> Result<bool, StoreError> {
    let Some(row) = tx.story(project, story)? else {
        return Ok(false);
    };
    if row.state != crate::service::verification::RETURNED_STATE
        || authority::policy_hold(tx, project, &row.snapshot)?.is_some()
        || super::resume::resource_hold(tx, project, story)?
    {
        return Ok(false);
    }
    for record in tx.project_recoveries(project)? {
        let view = persistence::read_view(tx, record)?;
        let assessment = &view.state.assessment;
        if view.record.active && assessment.story == story {
            let waiting = match assessment.status {
                AssessmentStatus::Pending | AssessmentStatus::InFlight => true,
                AssessmentStatus::Delivered => {
                    let at = assessment.delivered_at.as_deref().ok_or_else(|| {
                        StoreError::Corrupt("delivered assessment has no timestamp".into())
                    })?;
                    persistence::timestamp(now)? - persistence::timestamp(at)?
                        < chrono::Duration::minutes(30)
                }
                _ => false,
            };
            if waiting && authority::assessment_hold(tx, &view)?.is_none() {
                return Ok(true);
            }
        }
        for work in &view.state.work {
            if work.story == story
                && matches!(work.status, WorkStatus::Pending | WorkStatus::InFlight)
                && (view.record.active || work.kind == WorkKind::Resume)
                && super::work::permitted(tx, &view, work)?.is_none()
            {
                return Ok(true);
            }
        }
        if let Some(decision) = &view.state.decision {
            for hold in decision
                .dependency_holds
                .iter()
                .filter(|h| h.story == story)
            {
                let subject = view
                    .state
                    .subjects
                    .iter()
                    .find(|s| {
                        s.story == story
                            && s.candidate.verifying_generation == Some(hold.generation)
                    })
                    .ok_or_else(|| {
                        StoreError::Corrupt("recovery dependency has no subject".into())
                    })?;
                if subject.returned
                    && row.awaiting.as_deref() == Some(&hold.awaiting)
                    && super::resume::awaiting_revision(tx, project, story)? == Some(hold.event)
                    && authority::state_revision(tx, project, story)? == subject.state_revision
                    && authority::label_revision(tx, project, story)? == subject.label_revision
                    && crate::service::verification::verifying_entry(tx, project, story)?
                        .map(|(_, generation)| generation)
                        == Some(hold.generation)
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}
