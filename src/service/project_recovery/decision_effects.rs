//! Story mutations inside the decision owner's single block-aware transaction.

use super::{
    AffectedSubmission, DecisionInput, DecisionReceipt, RecoveryView, RepairScope, authority,
};
use crate::{
    domain::StoryEvent,
    service::{Ctx, NewStoryInput, append_and_fold, project_prefix},
    store::{EventSeq, ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo, WriteOps},
};

pub(super) fn create_repair<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    view: &RecoveryView,
    input: &DecisionInput,
    now: &str,
) -> Result<StoryNo, StoreError> {
    let spec = input
        .repair
        .as_ref()
        .ok_or_else(|| StoreError::Validation("separate repair has no specification".into()))?;
    let description = format!(
        "{}\n\nAcceptance criteria:\n{}\n\nRecovery: {}\n{}\n\nPreserve all required test coverage. Run new and impacted tests, commit, and submit to central verification. Do not create another repair story for a fault in this repair; retain the same recovery lineage.",
        spec.description,
        spec.acceptance,
        view.record.id,
        input.comment(&view.record.id)
    );
    let project = view.record.project;
    let states = tx.states(project)?;
    let events = crate::service::story::creation_events(
        tx,
        project,
        &states,
        &NewStoryInput {
            title: spec.title.clone(),
            story_type: Some("bug".into()),
            priority: Some("critical".into()),
            description: Some(description),
            ..Default::default()
        },
        now,
    )?;
    let story = tx.allocate_story_no(project)?;
    append_and_fold(
        tx,
        project,
        story,
        &project_prefix(tx, project)?,
        &tx.state_map(project)?,
        ExpectedSeq::Exact(EventSeq::ZERO),
        &events,
        ctx.provenance(),
    )?;
    Ok(story)
}

pub(super) fn apply<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    view: &RecoveryView,
    receipt: &mut DecisionReceipt,
    now: &str,
) -> Result<(), StoreError> {
    let project = view.record.project;
    let prefix = project_prefix(tx, project)?;
    for subject in &view.state.subjects {
        if !subject_is_current(tx, project, subject)? {
            receipt.skipped_subjects.push(subject.story);
            continue;
        }
        let mut events = vec![StoryEvent::StoryCommentAdded {
            at: now.into(),
            text: receipt.input.comment(&view.record.id),
        }];
        if let Some(repair) = receipt.repair_story {
            if repair != subject.story {
                // A fresh repair has no dependencies. Same-story scope still must
                // prove that attaching another affected story cannot form a cycle.
                if reaches(tx, project, repair, subject.story)? {
                    return Err(StoreError::Validation(
                        "recovery dependency would form a cycle".into(),
                    ));
                }
                let repair_id = repair.to_id(&prefix);
                let row = tx
                    .story(project, subject.story)?
                    .ok_or_else(|| StoreError::Corrupt("recovery subject disappeared".into()))?;
                if !row
                    .snapshot
                    .relationships
                    .iter()
                    .any(|r| r.relation == "blocked-by" && r.other_id == repair_id)
                {
                    events.push(StoryEvent::StoryRelationshipAdded {
                        at: now.into(),
                        other_id: repair_id.clone(),
                        relation: "blocked-by".into(),
                    });
                    append(
                        tx,
                        ctx,
                        repair,
                        &[StoryEvent::StoryRelationshipAdded {
                            at: now.into(),
                            other_id: subject.story.to_id(&prefix),
                            relation: "blocks".into(),
                        }],
                    )?;
                    receipt.owned_edges.push(subject.story);
                }
                events.push(StoryEvent::StoryAwaitingSet {
                    at: now.into(),
                    awaiting: format!(
                        "Project recovery {}: wait for certified repair landing of {}",
                        view.record.id, repair_id
                    ),
                });
            } else {
                events.push(StoryEvent::StoryCommentAdded { at: now.into(), text: format!("Repair this fault in this story and worktree. Read recovery {}. Preserve required coverage, run new and impacted tests, commit changed input, and resubmit to central verification.", view.record.id) });
            }
        } else if receipt.input.scope == RepairScope::External {
            events.push(StoryEvent::StoryAwaitingSet {
                at: now.into(),
                awaiting: format!(
                    "Project recovery {}: {}",
                    view.record.id,
                    receipt.input.prerequisite.as_deref().unwrap_or_default()
                ),
            });
        }
        append(tx, ctx, subject.story, &events)?;
        if receipt
            .repair_story
            .is_some_and(|repair| repair != subject.story)
        {
            let (event, awaiting) = tx
                .events_for(project, subject.story)?
                .into_iter()
                .rev()
                .find_map(|event| {
                    if let Some(StoryEvent::StoryAwaitingSet { awaiting, .. }) = event.known() {
                        Some((event.global_seq, awaiting.clone()))
                    } else {
                        None
                    }
                })
                .ok_or_else(|| {
                    StoreError::Corrupt("recovery dependency hold event missing".into())
                })?;
            receipt.dependency_holds.push(super::OwnedDependencyHold {
                story: subject.story,
                generation: subject.candidate.verifying_generation.ok_or_else(|| {
                    StoreError::Corrupt("recovery dependency has no generation".into())
                })?,
                awaiting,
                event,
            });
        }
    }
    Ok(())
}

fn subject_is_current(
    tx: &impl ReadOps,
    project: ProjectId,
    subject: &AffectedSubmission,
) -> Result<bool, StoreError> {
    let Some(row) = tx.story(project, subject.story)? else {
        return Ok(false);
    };
    Ok(subject.returned
        && row.state == crate::service::verification::RETURNED_STATE
        && row.awaiting.is_none()
        && authority::policy_hold(tx, project, &row.snapshot)?.is_none()
        && authority::state_revision(tx, project, subject.story)? == subject.state_revision
        && authority::label_revision(tx, project, subject.story)? == subject.label_revision
        && !tx.story_resets(project)?.contains_key(&subject.story)
        && tx
            .story_reset(project, subject.story)?
            .is_none_or(|r| r.completed)
        && tx.engine_reset(project, subject.story)?.is_none()
        && !tx
            .landing_intents()?
            .iter()
            .any(|i| i.project == project && i.story == subject.story))
}

fn append<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    story: StoryNo,
    events: &[StoryEvent],
) -> Result<(), StoreError> {
    let project = ctx.project();
    let row = tx
        .story(project, story)?
        .ok_or_else(|| StoreError::Corrupt("recovery story disappeared".into()))?;
    append_and_fold(
        tx,
        project,
        story,
        &project_prefix(tx, project)?,
        &tx.state_map(project)?,
        ExpectedSeq::Exact(row.head_seq),
        events,
        ctx.provenance(),
    )?;
    Ok(())
}

fn reaches(
    tx: &impl ReadOps,
    project: ProjectId,
    start: StoryNo,
    target: StoryNo,
) -> Result<bool, StoreError> {
    let prefix = project_prefix(tx, project)?;
    let mut pending = vec![start];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(story) = pending.pop() {
        if story == target {
            return Ok(true);
        }
        if !seen.insert(story) {
            continue;
        }
        if let Some(row) = tx.story(project, story)? {
            for edge in row
                .snapshot
                .relationships
                .iter()
                .filter(|r| r.relation == "blocked-by")
            {
                pending.push(StoryNo::parse_id(&prefix, &edge.other_id)?);
            }
        }
    }
    Ok(false)
}
