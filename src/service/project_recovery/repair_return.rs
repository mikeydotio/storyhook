//! Each completed failed repair owns a separate bounded delivery receipt.

use super::{
    AssessmentHold, RecoveryView, RepairCompletion, WorkDelivery, WorkKind, WorkStatus, authority,
};
use crate::{
    service::Ctx,
    store::{Store, StoreError, WriteOps},
};

pub(super) fn enqueue<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    view: &mut RecoveryView,
    attempt: &str,
    now: &str,
) -> Result<(), StoreError> {
    let Some(admitted) = view.state.attempts.iter().find(|a| {
        a.id == attempt
            && matches!(
                a.completion,
                Some(RepairCompletion::ProjectFault | RepairCompletion::TestsFailed)
            )
    }) else {
        return Ok(());
    };
    let id = identity(&view.record.id, attempt);
    if view.state.work.iter().any(|w| w.id == id) {
        return Ok(());
    }
    let subject = view
        .state
        .subjects
        .iter()
        .find(|s| {
            s.story == admitted.story
                && s.candidate.verifying_generation == Some(admitted.generation)
        })
        .ok_or_else(|| StoreError::Corrupt("recursive fault has no affected submission".into()))?;
    if !subject.returned {
        return Ok(());
    }
    let exhausted = view
        .state
        .attempts
        .iter()
        .filter(|a| a.completion.is_some())
        .map(|a| &a.input.head_tree)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        >= 3;
    let story = admitted.story;
    let row = tx
        .story(view.record.project, story)?
        .ok_or_else(|| StoreError::Corrupt("recursive repair disappeared".into()))?;
    view.state.work.push(WorkDelivery {
        id, story, kind: WorkKind::SameStoryRepair, source_attempt: Some(attempt.into()), state: row.state,
        state_revision: authority::state_revision(tx, view.record.project, story)?,
        label_revision: authority::label_revision(tx, view.record.project, story)?,
        release_event: None, disposition: None,
        status: if exhausted { WorkStatus::Held } else { WorkStatus::Pending },
        hold: exhausted.then_some(AssessmentHold::RepairExhausted),
        epoch: 0, failures: 0, started_at: None, delivered_at: None, last_result: None,
        detail: if exhausted { AssessmentHold::RepairExhausted.detail().into() } else { format!("repair attempt {attempt} did not resolve the recovery; resume the same repair lineage and worktree") },
    });
    super::work_holds::record(tx, ctx, view, view.state.work.len() - 1, now)
}

fn identity(recovery: &str, attempt: &str) -> String {
    format!("{recovery}:repair:{attempt}")
}

pub(super) fn owns(state: &super::RecoveryState, work: &WorkDelivery) -> bool {
    work.kind == WorkKind::SameStoryRepair
        && state
            .decision
            .as_ref()
            .is_some_and(|d| d.repair_story == Some(work.story))
        && work.source_attempt.as_ref().is_some_and(|id| {
            state.attempts.iter().any(|a| {
                &a.id == id
                    && a.story == work.story
                    && matches!(
                        a.completion,
                        Some(RepairCompletion::ProjectFault | RepairCompletion::TestsFailed)
                    )
            })
        })
}

pub(super) fn validate(view: &RecoveryView) -> Result<(), StoreError> {
    for work in view
        .state
        .work
        .iter()
        .filter(|w| w.source_attempt.is_some())
    {
        let id = work.source_attempt.as_deref().unwrap_or_default();
        let admitted = view.state.attempts.iter().find(|a| a.id == id);
        let valid = owns(&view.state, work)
            && work.id == identity(&view.record.id, id)
            && admitted.is_some_and(|a| {
                (a.completion == Some(RepairCompletion::TestsFailed)
                    || view.observations.iter().any(|o| {
                        o.attempt_id == id && o.story == work.story && o.generation == a.generation
                    }))
                    && view.state.subjects.iter().any(|s| {
                        s.returned
                            && s.story == work.story
                            && s.candidate.verifying_generation == Some(a.generation)
                            && s.state_revision == work.state_revision
                            && s.label_revision == work.label_revision
                    })
            });
        if !valid {
            return Err(StoreError::Corrupt(
                "recursive repair delivery has inconsistent observation or submission authority"
                    .into(),
            ));
        }
    }
    Ok(())
}
