//! Terminal delivery diagnostics are ordinary, exactly owned story holds.

use super::{RecoveryView, WorkStatus, authority, persistence};
use crate::{
    domain::StoryEvent,
    service::{Ctx, append_and_fold, project_prefix},
    store::{ExpectedSeq, GlobalSeq, ReadOps, Store, StoreError, WriteOps},
};
use serde::{Deserialize, Serialize};

/// Exact awaiting event created when managed work delivery became held.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkHold {
    /// Awaiting text, distinct from independent operator holds.
    pub awaiting: String,
    /// Sequence of this effect's awaiting write.
    pub event: GlobalSeq,
    /// RFC3339 time of the story write.
    pub at: String,
}

pub(super) fn record<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    view: &mut RecoveryView,
    index: usize,
    now: &str,
) -> Result<(), StoreError> {
    let work = &view.state.work[index];
    if work.status != WorkStatus::Held || work.disposition.is_some() {
        return Ok(());
    }
    let project = view.record.project;
    let Some(row) = tx.story(project, work.story)? else {
        return Ok(());
    };
    if row.state != work.state
        || row.awaiting.is_some()
        || authority::state_revision(tx, project, work.story)? != work.state_revision
        || authority::label_revision(tx, project, work.story)? != work.label_revision
        || authority::blocking_revision(tx, project, work.story)? != work.blocking_revision
        || row
            .snapshot
            .labels
            .iter()
            .any(|label| label == "human-only" || label == "no-auto")
        || super::resume::resource_hold(tx, project, work.story)?
    {
        return Ok(());
    }
    let cause = work
        .hold
        .ok_or_else(|| StoreError::Corrupt("held delivery has no cause".into()))?;
    let awaiting = format!("Project recovery {}: {}", view.record.id, cause.detail());
    let comment = format!(
        "PROJECT RECOVERY DELIVERY HELD — {}\nEffect: {}. Proven failures: {}/3. {}\nRead `story verifier repair show {} --json` for fault, attempt, and ownership evidence. No competing managed delivery will be launched.",
        cause.detail(),
        work.id,
        work.failures,
        crate::text_lint::quote_evidence(&work.detail),
        view.record.id
    );
    append_and_fold(
        tx,
        project,
        work.story,
        &project_prefix(tx, project)?,
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
    let event = super::resume::awaiting_revision(tx, project, work.story)?
        .ok_or_else(|| StoreError::Corrupt("work delivery hold event missing".into()))?;
    view.state.work[index].disposition = Some(WorkHold {
        awaiting,
        event,
        at: now.into(),
    });
    Ok(())
}

pub(super) fn validate(tx: &impl ReadOps, view: &RecoveryView) -> Result<(), StoreError> {
    for work in &view.state.work {
        if let Some(hold) = &work.disposition {
            persistence::timestamp(&hold.at)?;
            if work.status != WorkStatus::Held || hold.event <= work.state_revision
                || !tx.events_for(view.record.project, work.story)?.iter().any(|event| event.global_seq == hold.event && matches!(event.known(), Some(StoryEvent::StoryAwaitingSet { at, awaiting }) if at == &hold.at && awaiting == &hold.awaiting))
            { return Err(StoreError::Corrupt("work delivery hold has inconsistent event authority".into())); }
        }
    }
    Ok(())
}
