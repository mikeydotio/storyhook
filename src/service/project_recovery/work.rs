//! Durable managed delivery effects, distinct from verifier workspace ownership.

use super::{
    AssessmentDelivery, AssessmentHold, ProjectRecoveryService, RecoveryView, RepairScope,
    authority, persistence,
};
use crate::{
    error::AppError,
    store::{GlobalSeq, ReadOps, Store, StoreError, StoryNo},
};
use serde::{Deserialize, Serialize};

/// The authorized work named in a managed delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkKind {
    /// Repair within the retained original worktree.
    SameStoryRepair,
    /// Start the dedicated repair story through managed dispatch.
    SeparateRepair,
    /// Refresh and resubmit an affected story after verified repair landing.
    Resume,
}

/// External effect lifecycle; an in-flight effect requires ownership reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkStatus {
    /// No external call is currently owned.
    Pending,
    /// One claimed call is outstanding or requires restart reconciliation.
    InFlight,
    /// Managed delivery was confirmed.
    Delivered,
    /// Policy, authority, ownership, or exhausted failures prevent delivery.
    Held,
}

/// A stable, bounded intent to deliver repair or resume instructions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkDelivery {
    /// Stable identity retained across retries and daemon restart.
    pub id: String,
    /// Target story in the recovery's proven owning project.
    pub story: StoryNo,
    /// Instructions and managed dispatch mode.
    pub kind: WorkKind,
    /// Completed recursive fault that authorizes this return to the repair owner.
    #[serde(default)]
    pub source_attempt: Option<String>,
    /// Exact lease of an independently managed claim accepted at delivery admission.
    #[serde(default)]
    pub managed_lease: Option<crate::domain::StoryCleanupLease>,
    /// State at authorization, before any external claim.
    pub state: String,
    /// Latest creation or state transition event at authorization.
    pub state_revision: GlobalSeq,
    /// Latest reserved-label event, including transient reservations.
    pub label_revision: Option<GlobalSeq>,
    /// Latest interruption when this effect was authorized, including cleared holds.
    #[serde(default)]
    pub blocking_revision: Option<i64>,
    /// Exact awaiting-clear event for a certified landing resume.
    #[serde(default)]
    pub release_event: Option<GlobalSeq>,
    /// Durable transport lifecycle.
    pub status: WorkStatus,
    /// Structured reason for a held effect.
    pub hold: Option<AssessmentHold>,
    /// Exact story hold retained on terminal delivery, if original authority survived.
    #[serde(default)]
    pub disposition: Option<super::WorkHold>,
    /// Claimed external-call ordinal.
    pub epoch: u32,
    /// Proven failures, excluding ambiguous operations.
    pub failures: u8,
    /// RFC3339 claim time.
    pub started_at: Option<String>,
    /// RFC3339 confirmed delivery time.
    pub delivered_at: Option<String>,
    /// Most recent exact completion for idempotent replay.
    pub last_result: Option<AssessmentDelivery>,
    /// Diagnostic evidence; classification uses typed state and hold fields.
    pub detail: String,
}

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Claim one pending repair or resume effect under current target authority.
    pub fn claim_work(
        &self,
        recovery: &str,
        effect: &str,
    ) -> Result<Option<RecoveryView>, AppError> {
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            let mut view = persistence::find(tx, self.ctx.project(), recovery)?;
            let index = work_index(&view, effect)?;
            if (!view.record.active && view.state.work[index].kind != WorkKind::Resume) || view.state.work[index].status != WorkStatus::Pending { return Ok(None); }
            if let Some(reason) = permitted(tx, &view, &view.state.work[index])? {
                let work = &mut view.state.work[index]; work.status = WorkStatus::Held;
                work.hold = Some(reason); work.detail = reason.detail().into();
                super::work_holds::record(tx, self.ctx, &mut view, index, &now)?;
                persistence::save(tx, &mut view, &now)?; return Ok(None);
            }
            let managed_lease = super::managed_claim::lease(tx, &view, &view.state.work[index])?;
            let work = &mut view.state.work[index];
            work.managed_lease = managed_lease;
            work.status = WorkStatus::InFlight; work.hold = None;
            work.epoch = work.epoch.checked_add(1).ok_or_else(|| StoreError::Corrupt("recovery work epoch overflow".into()))?;
            work.started_at = Some(now.clone()); work.last_result = None;
            work.detail = "managed delivery claimed; external result or ownership reconciliation required".into();
            persistence::save(tx, &mut view, &now)?;
            Ok(Some(view))
        }).map_err(Into::into)
    }
    /// Settle the exact claimed managed delivery; duplicate completion is read-only.
    pub fn settle_work(
        &self,
        recovery: &str,
        effect: &str,
        epoch: u32,
        result: AssessmentDelivery,
    ) -> Result<RecoveryView, AppError> {
        if matches!(&result, AssessmentDelivery::ProvenFailure(detail) | AssessmentDelivery::Uncertain(detail) if detail.trim().is_empty())
        {
            return Err(AppError::Validation(
                "managed delivery failure requires diagnostics".into(),
            ));
        }
        let now = self.ctx.now();
        self.ctx
            .write_stories(|tx| {
                let mut view = persistence::find(tx, self.ctx.project(), recovery)?;
                let index = work_index(&view, effect)?;
                let work = &mut view.state.work[index];
                if work.epoch != epoch {
                    return Err(StoreError::Validation(
                        "stale recovery work delivery epoch".into(),
                    ));
                }
                if work.last_result.as_ref() == Some(&result) {
                    return Ok(view);
                }
                if work.status != WorkStatus::InFlight {
                    return Err(StoreError::Validation(
                        "recovery work delivery is not in flight".into(),
                    ));
                }
                match &result {
                    AssessmentDelivery::Delivered => {
                        work.status = WorkStatus::Delivered;
                        work.delivered_at = Some(now.clone());
                        work.detail = "managed work instructions delivered".into();
                    }
                    AssessmentDelivery::ProvenFailure(detail) => {
                        work.failures = work.failures.saturating_add(1);
                        work.status = if work.failures >= 3 {
                            WorkStatus::Held
                        } else {
                            WorkStatus::Pending
                        };
                        work.hold =
                            (work.failures >= 3).then_some(AssessmentHold::DeliveryExhausted);
                        work.detail = format!(
                            "proven managed delivery failure {}/3: {detail}",
                            work.failures
                        );
                    }
                    AssessmentDelivery::Uncertain(detail) => {
                        work.status = WorkStatus::Held;
                        work.hold = Some(AssessmentHold::OwnershipUncertain);
                        work.detail = detail.clone();
                    }
                }
                work.last_result = Some(result);
                // A successful helper may itself claim/advance the story. Completion
                // retains transport evidence; only still-pending retry needs the old state.
                let held = if view.state.work[index].status == WorkStatus::Pending {
                    permitted(tx, &view, &view.state.work[index])?
                } else {
                    policy(tx, &view, &view.state.work[index])?
                };
                if let Some(reason) = held {
                    let work = &mut view.state.work[index];
                    work.status = WorkStatus::Held;
                    work.hold = Some(reason);
                    work.detail
                        .push_str(&format!("; authority withheld: {}", reason.detail()));
                }
                super::work_holds::record(tx, self.ctx, &mut view, index, &now)?;
                persistence::save(tx, &mut view, &now)?;
                Ok(view)
            })
            .map_err(Into::into)
    }
}

pub(super) fn enqueue_repair(tx: &impl ReadOps, view: &mut RecoveryView) -> Result<(), StoreError> {
    let Some(receipt) = &view.state.decision else {
        return Ok(());
    };
    let (Some(story), Some(id)) = (receipt.repair_story, receipt.delivery_identity.as_ref()) else {
        return Ok(());
    };
    if view.state.work.iter().any(|work| &work.id == id) {
        return Ok(());
    }
    let row = tx
        .story(view.record.project, story)?
        .ok_or_else(|| StoreError::Corrupt("accepted repair story disappeared".into()))?;
    view.state.work.push(WorkDelivery {
        id: id.clone(),
        story,
        kind: if receipt.input.scope == RepairScope::SameStory {
            WorkKind::SameStoryRepair
        } else {
            WorkKind::SeparateRepair
        },
        source_attempt: None,
        managed_lease: None,
        state: row.state,
        state_revision: authority::state_revision(tx, view.record.project, story)?,
        label_revision: authority::label_revision(tx, view.record.project, story)?,
        blocking_revision: authority::blocking_revision(tx, view.record.project, story)?,
        release_event: None,
        status: WorkStatus::Pending,
        hold: None,
        disposition: None,
        epoch: 0,
        failures: 0,
        started_at: None,
        delivered_at: None,
        last_result: None,
        detail: "managed repair delivery pending outside verifier ownership".into(),
    });
    Ok(())
}

fn work_index(view: &RecoveryView, id: &str) -> Result<usize, StoreError> {
    view.state
        .work
        .iter()
        .position(|work| work.id == id)
        .ok_or_else(|| {
            StoreError::Validation("recovery has no matching work delivery identity".into())
        })
}

pub(super) fn validate(state: &super::RecoveryState) -> Result<(), StoreError> {
    let mut ids = std::collections::BTreeSet::new();
    for work in &state.work {
        let owned = state.decision.as_ref().is_some_and(|decision| {
            decision.delivery_identity.as_deref() == Some(&work.id)
                && decision.repair_story == Some(work.story)
                && matches!(
                    (decision.input.scope, work.kind),
                    (RepairScope::SameStory, WorkKind::SameStoryRepair)
                        | (RepairScope::SeparateStory, WorkKind::SeparateRepair)
                )
        });
        if !(owned
            || super::repair_return::owns(state, work)
            || (work.kind == WorkKind::Resume
                && state.landing.is_some()
                && work.release_event.is_some()))
            || (work.kind != WorkKind::Resume && work.release_event.is_some())
            || work.id.trim().is_empty()
            || !ids.insert(&work.id)
            || work.state.trim().is_empty()
            || work.failures > 3
            || u32::from(work.failures) > work.epoch
            || (work.status == WorkStatus::Held) != work.hold.is_some()
            || (work.epoch > 0 && work.started_at.is_none())
            || (work.status == WorkStatus::InFlight
                && (work.epoch == 0 || work.last_result.is_some()))
            || (work.status == WorkStatus::Delivered
                && (work.delivered_at.is_none()
                    || work.last_result != Some(AssessmentDelivery::Delivered)))
        {
            return Err(StoreError::Corrupt(
                "project recovery work has inconsistent identity, authority, or delivery evidence"
                    .into(),
            ));
        }
    }
    Ok(())
}

fn policy(
    tx: &impl ReadOps,
    view: &RecoveryView,
    work: &WorkDelivery,
) -> Result<Option<AssessmentHold>, StoreError> {
    let project = view.record.project;
    let Some(row) = tx.story(project, work.story)? else {
        return Ok(Some(AssessmentHold::SubjectMissing));
    };
    if let Some(reason) = authority::policy_hold(tx, project, &row.snapshot)? {
        return Ok(Some(reason));
    }
    if authority::label_revision(tx, project, work.story)? != work.label_revision
        || authority::blocking_revision(tx, project, work.story)? != work.blocking_revision
    {
        return Ok(Some(AssessmentHold::AuthorityChanged));
    }
    Ok(None)
}

pub(super) fn permitted(
    tx: &impl ReadOps,
    view: &RecoveryView,
    work: &WorkDelivery,
) -> Result<Option<AssessmentHold>, StoreError> {
    if let Some(reason) = policy(tx, view, work)? {
        return Ok(Some(reason));
    }
    let project = view.record.project;
    let row = tx
        .story(project, work.story)?
        .ok_or_else(|| StoreError::Corrupt("recovery work target disappeared".into()))?;
    if row.state != work.state
        || authority::state_revision(tx, project, work.story)? != work.state_revision
    {
        let managed = super::managed_claim::lease(tx, view, work)?;
        if managed.is_none()
            || work
                .managed_lease
                .as_ref()
                .is_some_and(|retained| Some(retained) != managed.as_ref())
        {
            return Ok(Some(AssessmentHold::AuthorityChanged));
        }
    }
    if (work.kind == WorkKind::Resume
        && super::resume::awaiting_revision(tx, project, work.story)? != work.release_event)
        || row.awaiting.is_some()
        || crate::domain::is_blocked(
            &row.snapshot,
            &crate::service::query::story_map(tx, project)?,
        )
        || super::resume::resource_hold(tx, project, work.story)?
    {
        return Ok(Some(AssessmentHold::ResourceOrDependency));
    }
    Ok(None)
}
