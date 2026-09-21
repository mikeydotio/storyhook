//! Managed recovery delivery runs independently of verifier queue ownership.
mod transport;

use super::verification::{ShellVerificationActuator, VerificationActivity};
use crate::{
    env::Environment,
    error::AppError,
    service::workspace_lock::WorkspaceLock,
    service::{
        Ctx,
        project_recovery::{
            AssessmentDelivery, AssessmentStatus, ProjectRecoveryService, RecoveryView, WorkKind,
            WorkStatus,
        },
    },
    store::{ReadOps, Store, StoryNo},
};
use std::sync::atomic::{AtomicBool, Ordering};

struct Operation {
    effect: Option<String>,
    story: StoryNo,
    epoch: u32,
    interrupted: bool,
    kind: Option<WorkKind>,
}

impl Operation {
    fn next(view: &RecoveryView, now: &str) -> Result<Option<Self>, AppError> {
        let assessment = &view.state.assessment;
        if view.record.active {
            let expired = if assessment.status == AssessmentStatus::Delivered {
                let at = assessment.delivered_at.as_deref().ok_or_else(|| {
                    AppError::Storage("delivered recovery assessment has no timestamp".into())
                })?;
                let parse = |value| {
                    chrono::DateTime::parse_from_rfc3339(value).map_err(|e| {
                        AppError::Storage(format!("recovery assessment timestamp {value}: {e}"))
                    })
                };
                parse(now)? - parse(at)? >= chrono::Duration::minutes(30)
            } else {
                false
            };
            if expired
                || matches!(
                    assessment.status,
                    AssessmentStatus::Pending | AssessmentStatus::InFlight
                )
            {
                return Ok(Some(Self {
                    effect: None,
                    story: assessment.story,
                    epoch: assessment.epoch,
                    interrupted: assessment.status == AssessmentStatus::InFlight,
                    kind: None,
                }));
            }
        }
        Ok(view
            .state
            .work
            .iter()
            .find(|w| {
                matches!(w.status, WorkStatus::Pending | WorkStatus::InFlight)
                    && (view.record.active || w.kind == WorkKind::Resume)
            })
            .map(|w| Self {
                effect: Some(w.id.clone()),
                story: w.story,
                epoch: w.epoch,
                interrupted: w.status == WorkStatus::InFlight,
                kind: Some(w.kind),
            }))
    }

    fn settle(
        &self,
        service: &ProjectRecoveryService<'_, impl Store>,
        view: &RecoveryView,
        result: AssessmentDelivery,
    ) -> Result<(), AppError> {
        if let Some(effect) = &self.effect {
            service.settle_work(&view.record.id, effect, self.epoch, result)?;
        } else {
            service.settle_assessment(
                &view.record.id,
                &view.state.assessment.dispatch_identity,
                self.epoch,
                result,
            )?;
        }
        Ok(())
    }
}

/// Process one durable recovery effect, without acquiring a verifier slot.
/// Busy stories do not prevent independent recovery records from advancing.
pub fn process_one(
    store: &impl Store,
    env: &Environment,
    actuator: &ShellVerificationActuator,
    activity: &VerificationActivity,
    stop: &AtomicBool,
) -> Result<bool, AppError> {
    let records = store.read(|tx| {
        let mut records = Vec::new();
        for project in tx.projects()? {
            if let Some(checkout) = tx.checkout_path(project.id)? {
                records.extend(
                    tx.project_recoveries(project.id)?
                        .into_iter()
                        .map(|r| (r, checkout.clone())),
                );
            }
        }
        Ok(records)
    })?;
    let mut errors = Vec::new();
    for (record, checkout) in records {
        if stop.load(Ordering::Acquire) {
            return Ok(false);
        }
        let ctx = Ctx::new(store, record.project, checkout, env.clone()).no_hooks(true);
        match process_record(&ctx, actuator, activity, stop, &record.id) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) => {
                super::activity::context::project_error(
                    store,
                    record.project,
                    "project-recovery",
                    &format!("recovery {}: {error}", record.id),
                );
                errors.push(format!("recovery {}: {error}", record.id));
            }
        }
    }
    if errors.is_empty() {
        Ok(false)
    } else {
        Err(AppError::Storage(errors.join("; ")))
    }
}

fn process_record(
    ctx: &Ctx<'_, impl Store>,
    actuator: &ShellVerificationActuator,
    activity: &VerificationActivity,
    stop: &AtomicBool,
    id: &str,
) -> Result<bool, AppError> {
    let service = ProjectRecoveryService::new(ctx);
    if service.landing_release_ready(id)? {
        service.reconcile_landing(id)?;
        return Ok(true);
    }
    let mut view = service.show(id)?;
    let Some(mut operation) = Operation::next(&view, &ctx.now())? else {
        return Ok(false);
    };
    if let Some(active) = activity.active_for(ctx.project())
        && view.state.subjects.iter().any(|s| {
            s.candidate.story_id == active.story_id
                && s.candidate.verifying_generation == active.generation
        })
    {
        return Ok(false);
    }
    let candidate = service.delivery_candidate(id, operation.story)?;
    let _log = super::activity::context::enter(super::activity::context::LogContext::candidate(
        &candidate, id,
    ));
    if operation.kind == Some(WorkKind::SeparateRepair)
        && !operation.interrupted
        && ctx.store().read(|tx| {
            for run in tx
                .live_engine_runs()?
                .iter()
                .filter(|r| r.project_slug == candidate.project_slug)
            {
                if tx.engine_lanes(&run.id)?.iter().any(|l| {
                    l.state == crate::store::EngineLaneState::Dispatching
                        && l.story_id.as_deref() == Some(&candidate.story_id)
                }) {
                    return Ok(true);
                }
            }
            Ok(false)
        })?
    {
        return Ok(false);
    }

    let Some(workspace) = WorkspaceLock::try_acquire(&candidate.checkout, &candidate.story_id)?
    else {
        return Ok(false);
    };
    // A dedicated repair has a different lock from the original faulting workspace.
    // Both must be free; inherited descriptors survive a dead daemon's children.
    let origin = &view.state.subjects[0].candidate;
    let _origin_workspace = if origin.story_id != candidate.story_id {
        let Some(lock) = WorkspaceLock::try_acquire(&origin.checkout, &origin.story_id)? else {
            return Ok(false);
        };
        Some(lock)
    } else {
        None
    };
    if !operation.interrupted {
        let claimed = if let Some(effect) = &operation.effect {
            service.claim_work(id, effect)?
        } else {
            service.claim_assessment(id)?
        };
        let Some(claimed) = claimed else {
            return Ok(true);
        };
        view = claimed;
        operation.epoch = if let Some(effect) = &operation.effect {
            view.state
                .work
                .iter()
                .find(|w| &w.id == effect)
                .expect("claimed existing work")
                .epoch
        } else {
            view.state.assessment.epoch
        };
    }
    let candidate = service.delivery_candidate(id, operation.story)?;
    let _log = super::activity::context::enter(super::activity::context::LogContext::candidate(
        &candidate, id,
    ));
    let result = if operation.interrupted {
        // Resource inspection can diagnose surviving ownership, but cannot prove
        // that an interrupted paste was never consumed. Never replay it blindly.
        let resources = transport::resources(ctx, &candidate);
        AssessmentDelivery::Uncertain(format!(
            "daemon stopped before the effect receipt; no automatic replay; target inspection: {}",
            match resources {
                Ok(r) => format!(
                    "{}; provider {:?}; pane {:?}; {}",
                    r.status,
                    r.provider,
                    r.pane.map(|p| p.pane_id),
                    r.diagnostics.join("; ")
                ),
                Err(e) => e.to_string(),
            }
        ))
    } else {
        transport::deliver(
            ctx, actuator, stop, &view, &operation, &candidate, &workspace,
        )
    };
    operation.settle(&service, &view, result)?;
    Ok(true)
}

/// Own recovery control calls independently of every blocking project gate.
pub(crate) fn poll(
    store: &impl Store,
    env: &Environment,
    bus: &super::bus::ChangeBus,
    stop: &AtomicBool,
    activity: &VerificationActivity,
) {
    let subscription = bus.subscribe();
    let actuator = ShellVerificationActuator::new(env.clone());
    while !stop.load(Ordering::Acquire) {
        match process_one(store, env, &actuator, activity, stop) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => eprintln!("storyhook: project recovery delivery: {error}"),
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !stop.load(Ordering::Acquire) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match subscription.recv(remaining) {
                Some(super::bus::Change::Ping) => {}
                _ => break,
            }
        }
    }
}
