//! Release exact recovery holds only after certified landing.

use super::{
    OwnedDependencyHold, ProjectRecoveryService, RecoveryView, WorkDelivery, WorkKind, WorkStatus,
    authority, persistence,
};
use crate::{
    domain::StoryEvent,
    error::AppError,
    service::{append_and_fold, project_prefix, verification::IMPLEMENTER_TEST_SCOPE},
    store::{ExpectedSeq, GlobalSeq, ProjectId, ReadOps, Store, StoreError, StoryNo},
};

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Reconcile the release authority and retain managed resume intents.
    pub fn reconcile_landing(&self, id: &str) -> Result<RecoveryView, AppError> {
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            let mut view = persistence::find(tx, self.ctx.project(), id)?;
            if view.state.shared.is_some() {
                if super::shared::readmit::reconcile(tx, self.ctx, &mut view, &now)? {
                    persistence::save(tx, &mut view, &now)?;
                }
                return Ok(view);
            }
            let Some(release) = super::resolution::release(&view.state) else { return Ok(view); };
            let cause = release.cause.clause();
            let holds = view.state.decision.as_ref().map(|d| d.dependency_holds.clone()).unwrap_or_default();
            let mut changed = false;
            for hold in holds {
                let effect = identity(&view.record.id, hold.event);
                if view.state.work.iter().any(|work| work.id == effect) || !eligible(tx, &view, &hold)? { continue; }
                let project = view.record.project;
                let row = tx.story(project, hold.story)?.ok_or_else(|| StoreError::Corrupt("resume subject disappeared".into()))?;
                append_and_fold(tx, project, hold.story, &project_prefix(tx, project)?, &tx.state_map(project)?,
                    ExpectedSeq::Exact(row.head_seq), &[
                        StoryEvent::StoryAwaitingCleared { at: now.clone() },
                        StoryEvent::StoryCommentAdded { at: now.clone(), text: format!("PROJECT RECOVERY {} — {cause}. Managed resume {} is pending. Refresh source and gate configuration from the current base, and reconcile the existing worktree. {IMPLEMENTER_TEST_SCOPE} Commit, and resubmit for a fresh central verification generation. The original submission remains unjudged.", view.record.id, effect) },
                    ], self.ctx.provenance())?;
                let release_event = awaiting_revision(tx, project, hold.story)?.ok_or_else(|| StoreError::Corrupt("recovery release event missing".into()))?;
                view.state.work.push(WorkDelivery {
                    id: effect, story: hold.story, kind: WorkKind::Resume, source_attempt: None, state: row.state,
                    state_revision: authority::state_revision(tx, project, hold.story)?,
                    label_revision: authority::label_revision(tx, project, hold.story)?,
                    blocking_revision: authority::blocking_revision(tx, view.record.project, hold.story)?, managed_lease: None, release_event: Some(release_event), status: WorkStatus::Pending, hold: None, disposition: None,
                    epoch: 0, failures: 0, started_at: None, delivered_at: None, last_result: None,
                    detail: format!("{cause}; managed affected-agent resume pending"),
                });
                changed = true;
            }
            if changed { persistence::save(tx, &mut view, &now)?; }
            Ok(view)
        }).map_err(Into::into)
    }
}

pub(super) fn eligible(
    tx: &impl ReadOps,
    view: &RecoveryView,
    hold: &OwnedDependencyHold,
) -> Result<bool, StoreError> {
    let project = view.record.project;
    let Some(subject) = view.state.subjects.iter().find(|subject| {
        subject.story == hold.story
            && subject.candidate.verifying_generation == Some(hold.generation)
    }) else {
        return Ok(false);
    };
    let Some(row) = tx.story(project, hold.story)? else {
        return Ok(false);
    };
    if !subject.returned
        || row.state != super::super::verification::RETURNED_STATE
        || row.awaiting.as_deref() != Some(&hold.awaiting)
        || awaiting_revision(tx, project, hold.story)? != Some(hold.event)
        || authority::state_revision(tx, project, hold.story)? != subject.state_revision
        || authority::label_revision(tx, project, hold.story)? != subject.label_revision
        || authority::policy_hold(tx, project, &row.snapshot)?.is_some()
        || super::super::verification::verifying_entry(tx, project, hold.story)?
            .map(|(_, generation)| generation)
            != Some(hold.generation)
        || resource_hold(tx, project, hold.story)?
    {
        return Ok(false);
    }
    let mut snapshot = row.snapshot;
    snapshot.awaiting = None;
    Ok(!crate::domain::is_blocked(
        &snapshot,
        &crate::service::query::story_map(tx, project)?,
    ))
}

pub(crate) fn resource_hold(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<bool, StoreError> {
    resource_hold_with_intent(tx, project, story, None)
}

/// The dedicated managed landing controller may continue its own exact durable
/// intent. This excludes no other landing, reset, or quarantined engine custody.
pub(crate) fn resource_hold_except_managed_landing(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    owned: &crate::store::LandingIntent,
) -> Result<bool, StoreError> {
    if owned.project != project
        || owned.story != story
        || owned.certification.integration().is_none()
        || !tx.landing_intents()?.contains(owned)
    {
        return Ok(true);
    }
    crate::service::integration_recovery::validate_landing_intent(tx, owned)?;
    resource_hold_with_intent(tx, project, story, Some(owned))
}

fn resource_hold_with_intent(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    owned: Option<&crate::store::LandingIntent>,
) -> Result<bool, StoreError> {
    if tx.story_resets(project)?.contains_key(&story)
        || tx
            .story_reset(project, story)?
            .is_some_and(|reset| !reset.completed)
        || tx.engine_reset(project, story)?.is_some()
        || tx.landing_intents()?.iter().any(|intent| {
            intent.project == project && intent.story == story && Some(intent) != owned
        })
    {
        return Ok(true);
    }
    let metadata = tx
        .project(project)?
        .ok_or_else(|| StoreError::Corrupt("recovery project disappeared".into()))?;
    let id = story.to_id(&project_prefix(tx, project)?);
    for run in tx.engine_runs(&metadata.slug)? {
        if tx.engine_lanes(&run.id)?.iter().any(|lane| {
            lane.story_id.as_deref() == Some(&id)
                && lane.state == crate::store::EngineLaneState::Quarantined
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn identity(recovery: &str, event: GlobalSeq) -> String {
    format!("{recovery}:resume:{}", event.get())
}

pub(super) fn awaiting_revision(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<GlobalSeq>, StoreError> {
    Ok(tx
        .events_for(project, story)?
        .iter()
        .rev()
        .find_map(|event| {
            matches!(
                event.known(),
                Some(StoryEvent::StoryAwaitingSet { .. } | StoryEvent::StoryAwaitingCleared { .. })
            )
            .then_some(event.global_seq)
        }))
}

pub(super) fn validate(tx: &impl ReadOps, view: &RecoveryView) -> Result<(), StoreError> {
    for work in view
        .state
        .work
        .iter()
        .filter(|work| work.kind == WorkKind::Resume)
    {
        let hold = view.state.decision.as_ref().and_then(|d| {
            d.dependency_holds
                .iter()
                .find(|h| h.story == work.story && work.id == identity(&view.record.id, h.event))
        });
        let valid = if let (Some(hold), Some(release), Some(authority)) = (
            hold,
            work.release_event,
            super::resolution::release(&view.state),
        ) {
            release > hold.event
                && release > authority.anchor
                && view.state.subjects.iter().any(|subject| {
                    subject.story == hold.story
                        && subject.candidate.verifying_generation == Some(hold.generation)
                        && subject.state_revision == work.state_revision
                        && subject.label_revision == work.label_revision
                })
                && tx
                    .events_for(view.record.project, work.story)?
                    .iter()
                    .any(|e| {
                        e.global_seq == release
                            && matches!(e.known(), Some(StoryEvent::StoryAwaitingCleared { .. }))
                    })
        } else {
            false
        };
        if !valid {
            return Err(StoreError::Corrupt(
                "recovery resume has inconsistent landing, hold, or release authority".into(),
            ));
        }
    }
    Ok(())
}

/// The recovery resume owns exactly its release event, never a later unblock episode.
pub(crate) fn owns_resume(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<bool, StoreError> {
    for view in super::references::views_naming(tx, project, story)? {
        for work in view
            .state
            .work
            .iter()
            .filter(|w| w.kind == WorkKind::Resume && w.story == story)
        {
            if work.release_event == awaiting_revision(tx, project, story)?
                && work.state_revision == authority::state_revision(tx, project, story)?
                && work.label_revision == authority::label_revision(tx, project, story)?
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
