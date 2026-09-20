//! Terminal assessment holds commit with exact event ownership and ordinary block edges.

use super::{OwnedAssessmentHold, RecoveryView, authority};
use crate::{
    domain::StoryEvent,
    service::{Ctx, append_and_fold, project_prefix},
    store::{ExpectedSeq, Store, StoreError, WriteOps},
};

pub(super) fn record<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    view: &mut RecoveryView,
    now: &str,
) -> Result<(), StoreError> {
    let Some(cause) = view.state.assessment.hold else {
        return Ok(());
    };
    let project = view.record.project;
    let prefix = project_prefix(tx, project)?;
    for subject in &view.state.subjects {
        let Some(row) = tx.story(project, subject.story)? else {
            continue;
        };
        if !subject.returned
            || view.state.holds.iter().any(|hold| {
                hold.story == subject.story
                    && Some(hold.generation) == subject.candidate.verifying_generation
            })
            || row.state != crate::service::verification::RETURNED_STATE
            || row.awaiting.is_some()
            || authority::state_revision(tx, project, subject.story)? != subject.state_revision
            || authority::label_revision(tx, project, subject.story)? != subject.label_revision
            || row
                .snapshot
                .labels
                .iter()
                .any(|label| label == "human-only" || label == "no-auto")
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
            continue;
        }
        let generation = subject.candidate.verifying_generation.ok_or_else(|| {
            StoreError::Corrupt("assessment hold subject has no generation".into())
        })?;
        let awaiting = format!("Project recovery {}: {}", view.record.id, cause.detail());
        let comment = format!(
            "PROJECT RECOVERY HELD — {}\n\n{}\nProven delivery failures: {}/3. Original submission generation: {}. Read `story verifier repair show {} --json` for immutable fault and delivery evidence. No competing agent will be launched.",
            view.record.id,
            view.state.assessment.detail,
            view.state.assessment.failures,
            generation.get(),
            view.record.id
        );
        append_and_fold(
            tx,
            project,
            subject.story,
            &prefix,
            &tx.state_map(project)?,
            ExpectedSeq::Exact(row.head_seq),
            &[
                StoryEvent::StoryCommentAdded {
                    at: now.into(),
                    text: comment,
                },
                StoryEvent::StoryAwaitingSet {
                    at: now.into(),
                    awaiting: awaiting.clone(),
                },
            ],
            ctx.provenance(),
        )?;
        let event = tx
            .events_for(project, subject.story)?
            .iter()
            .rev()
            .find_map(|event| {
                matches!(event.known(), Some(StoryEvent::StoryAwaitingSet { .. }))
                    .then_some(event.global_seq)
            })
            .ok_or_else(|| {
                StoreError::Corrupt("assessment hold awaiting event was not retained".into())
            })?;
        view.state.holds.push(OwnedAssessmentHold {
            story: subject.story,
            generation,
            cause,
            awaiting,
            event,
        });
    }
    Ok(())
}
