//! Read-only authority checks at external delivery boundaries.
use super::{
    AssessmentStatus, ProjectRecoveryService, WorkKind, WorkStatus, authority, persistence,
};
use crate::{
    domain::StoryEvent,
    error::AppError,
    service::VerificationCandidate,
    store::{ReadOps, Store, StoreError, StoryNo},
};

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Resolve a transport target without borrowing another story's submission or lease.
    pub fn delivery_candidate(
        &self,
        recovery: &str,
        story: StoryNo,
    ) -> Result<VerificationCandidate, AppError> {
        self.ctx
            .store()
            .read(|tx| {
                let view = persistence::find(tx, self.ctx.project(), recovery)?;
                let row = tx.story(self.ctx.project(), story)?.ok_or_else(|| {
                    StoreError::Validation("recovery delivery target disappeared".into())
                })?;
                let project = tx
                    .project(self.ctx.project())?
                    .ok_or_else(|| StoreError::Validation("recovery project disappeared".into()))?;
                let mut candidate = view
                    .state
                    .subjects
                    .iter()
                    .rev()
                    .find(|s| s.story == story)
                    .unwrap_or(&view.state.subjects[0])
                    .candidate
                    .clone();
                if candidate.story_id != row.snapshot.id {
                    candidate.verifying_since = None;
                    candidate.verifying_generation = None;
                    candidate.cleanup_lease = None;
                    candidate.pull_request =
                        Err(crate::service::VerificationProblem::MissingPullRequest);
                    candidate.blocking_revision = None;
                    candidate.human_only_revision = None;
                }
                if let Some(lease) = view
                    .state
                    .work
                    .iter()
                    .rev()
                    .find(|w| w.story == story)
                    .and_then(|w| w.managed_lease.as_ref())
                {
                    candidate.cleanup_lease = Some(lease.clone());
                }
                candidate.story_id = row.snapshot.id;
                candidate.title = row.title;
                candidate.priority = row.priority;
                candidate.created_at = row.created_at;
                candidate.project_slug = project.slug;
                candidate.checkout = tx.checkout_path(self.ctx.project())?.ok_or_else(|| {
                    StoreError::Validation("recovery checkout disappeared".into())
                })?;
                Ok(candidate)
            })
            .map_err(Into::into)
    }

    /// Check the exact claimed operation during a bounded provider call.
    /// Fresh dispatch may make one normal claim; later state changes revoke it.
    pub fn delivery_permitted(
        &self,
        recovery: &str,
        effect: Option<&str>,
        epoch: u32,
        dispatching: bool,
    ) -> Result<bool, AppError> {
        self.ctx
            .store()
            .read(|tx| {
                let view = persistence::find(tx, self.ctx.project(), recovery)?;
                let Some(effect) = effect else {
                    return Ok(view.state.assessment.epoch == epoch
                        && (view.state.assessment.status == AssessmentStatus::Decided
                            || (view.state.assessment.status == AssessmentStatus::InFlight
                                && authority::assessment_hold(tx, &view)?.is_none())));
                };
                let work = view
                    .state
                    .work
                    .iter()
                    .find(|w| w.id == effect)
                    .ok_or_else(|| {
                        StoreError::Validation("recovery delivery effect disappeared".into())
                    })?;
                if work.epoch != epoch || work.status != WorkStatus::InFlight {
                    return Ok(false);
                }
                if super::work::permitted(tx, &view, work)?.is_none() {
                    return Ok(true);
                }
                if !dispatching || work.kind != WorkKind::SeparateRepair {
                    return Ok(false);
                }
                let Some(row) = tx.story(view.record.project, work.story)? else {
                    return Ok(false);
                };
                if row.state != crate::service::verification::RETURNED_STATE
                    || row.awaiting.is_some()
                    || authority::policy_hold(tx, view.record.project, &row.snapshot)?.is_some()
                    || authority::label_revision(tx, view.record.project, work.story)?
                        != work.label_revision
                    || authority::blocking_revision(tx, view.record.project, work.story)?
                        != work.blocking_revision
                    || super::resume::resource_hold(tx, view.record.project, work.story)?
                    || crate::domain::is_blocked(
                        &row.snapshot,
                        &crate::service::query::story_map(tx, view.record.project)?,
                    )
                {
                    return Ok(false);
                }
                let events = tx.events_for(view.record.project, work.story)?;
                let mut transitions = 0;
                for event in events.iter().filter(|e| e.global_seq > work.state_revision) {
                    match event.known() {
                        Some(StoryEvent::StoryStateChanged { .. }) => transitions += 1,
                        Some(StoryEvent::StoryAwaitingSet { .. }) => return Ok(false),
                        _ => {}
                    }
                }
                Ok(transitions == 1)
            })
            .map_err(Into::into)
    }

    /// Avoid housekeeping write transactions when certified landing cannot release a hold.
    pub fn landing_release_ready(&self, recovery: &str) -> Result<bool, AppError> {
        self.ctx
            .store()
            .read(|tx| {
                let view = persistence::find(tx, self.ctx.project(), recovery)?;
                if view.state.landing.is_none() {
                    return Ok(false);
                }
                if let Some(decision) = &view.state.decision {
                    for hold in &decision.dependency_holds {
                        if super::resume::eligible(tx, &view, hold)? {
                            return Ok(true);
                        }
                    }
                }
                Ok(false)
            })
            .map_err(Into::into)
    }
}
