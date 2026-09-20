//! A concurrent engine claim can retain the approved repair, never replace its identity.
use super::{RecoveryView, WorkDelivery, WorkKind};
use crate::{
    domain::{StoryCleanupLease, StoryEvent},
    store::{EngineLaneState, ReadOps, StoreError},
};

pub(super) fn lease(
    tx: &impl ReadOps,
    view: &RecoveryView,
    work: &WorkDelivery,
) -> Result<Option<StoryCleanupLease>, StoreError> {
    if work.kind != WorkKind::SeparateRepair || work.source_attempt.is_some() {
        return Ok(None);
    }
    let project = tx
        .project(view.record.project)?
        .ok_or_else(|| StoreError::Corrupt("repair project disappeared".into()))?;
    let Some(row) = tx.story(view.record.project, work.story)? else {
        return Ok(None);
    };
    if row.state != crate::service::verification::RETURNED_STATE {
        return Ok(None);
    }
    let events = tx.events_for(view.record.project, work.story)?;
    let mut transitions = 0;
    for event in events.iter().filter(|e| e.global_seq > work.state_revision) {
        match event.known() {
            Some(StoryEvent::StoryStateChanged { .. }) => transitions += 1,
            Some(StoryEvent::StoryAwaitingSet { .. }) => return Ok(None),
            _ => {}
        }
    }
    if transitions != 1 {
        return Ok(None);
    }
    let mut leases = Vec::new();
    for run in tx
        .live_engine_runs()?
        .iter()
        .filter(|run| run.project_slug == project.slug)
    {
        for lane in tx.engine_lanes(&run.id)? {
            if lane.state != EngineLaneState::Working
                || lane.story_id.as_deref() != Some(&row.snapshot.id)
            {
                continue;
            }
            let Some(lease) = lane.cleanup_lease else {
                return Ok(None);
            };
            if lease.story_id != row.snapshot.id
                || lease.project_slug != project.slug
                || !valid_lease(&lease)
            {
                return Ok(None);
            }
            leases.push(lease);
        }
    }
    Ok(if leases.len() == 1 {
        leases.pop()
    } else {
        None
    })
}

/// Pure shape validation; current Git and pane identity belongs to the transport.
pub(super) fn valid_lease(lease: &StoryCleanupLease) -> bool {
    lease.version == crate::domain::CLEANUP_LEASE_VERSION
        && !lease.branch.trim().is_empty()
        && lease.repository_path.is_absolute()
        && lease.worktree_path.is_absolute()
        && lease.repository_path != lease.worktree_path
        && lease.tmux.socket_path.is_absolute()
}
