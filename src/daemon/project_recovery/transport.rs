//! Provider calls retain workspace exclusion and continuously checked authority.
use super::*;
use crate::daemon::verification::{ControlOwner, NotifyDelivery, resume_plan};
use crate::{
    process::Cancellation,
    service::{
        VerificationCandidate,
        resources::{ResourceOptions, ResourceReport, ResourceService},
    },
};
use std::{sync::mpsc, time::Duration};

pub(super) fn resources(
    ctx: &Ctx<'_, impl Store>,
    candidate: &VerificationCandidate,
) -> Result<ResourceReport, AppError> {
    ResourceService::new(ctx).resolve(
        &candidate.story_id,
        &ResourceOptions {
            lease_json: candidate
                .cleanup_lease
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            ..Default::default()
        },
    )
}

pub(super) fn deliver(
    ctx: &Ctx<'_, impl Store>,
    actuator: &ShellVerificationActuator,
    stop: &AtomicBool,
    view: &RecoveryView,
    operation: &Operation,
    candidate: &VerificationCandidate,
    workspace: &WorkspaceLock,
) -> AssessmentDelivery {
    let service = ProjectRecoveryService::new(ctx);
    let cancellation = Cancellation::default();
    let dispatching = AtomicBool::new(false);
    let (finished, completion) = mpsc::channel::<()>();
    let authority = || {
        service.delivery_permitted(
            &view.record.id,
            operation.effect.as_deref(),
            operation.epoch,
            dispatching.load(Ordering::Acquire),
        )
    };
    let result = std::thread::scope(|scope| {
        let token = &cancellation;
        let check_authority = &authority;
        let watcher = scope.spawn(move || {
            loop {
                let check = if stop.load(Ordering::Acquire) {
                    Ok(false)
                } else {
                    check_authority()
                };
                match check {
                    Ok(true) => {}
                    Ok(false) => {
                        token.cancel();
                        return Some("recovery delivery authority was revoked".to_string());
                    }
                    Err(error) => {
                        token.cancel();
                        return Some(format!("recovery delivery authority read failed: {error}"));
                    }
                }
                if completion.recv_timeout(Duration::from_millis(100))
                    != Err(mpsc::RecvTimeoutError::Timeout)
                {
                    return None;
                }
            }
        });
        let result = (|| -> Result<AssessmentDelivery, AppError> {
            if stop.load(Ordering::Acquire) || !authority()? {
                return Err(AppError::Validation(
                    "recovery delivery no longer permitted".into(),
                ));
            }
            let message = message(view, operation);
            match actuator.notify_owned(candidate, &message, Some(workspace), &cancellation)? {
                NotifyDelivery::Delivered => Ok(AssessmentDelivery::Delivered),
                NotifyDelivery::AgentAbsent { .. } => {
                    let report = resources(ctx, candidate)?;
                    if !matches!(report.status.as_str(), "resolved" | "absent") {
                        return Err(AppError::Validation(format!(
                            "recovery target resources {}: {}",
                            report.status,
                            report.diagnostics.join("; ")
                        )));
                    }
                    if report.pane.as_ref().is_some_and(|p| !p.dead) {
                        return Err(AppError::Validation(
                            "a live pane appeared after absence; do not replace it".into(),
                        ));
                    }
                    let lease = report.candidates.iter().find_map(|c| c.lease.as_ref());
                    let fresh = operation.kind == Some(WorkKind::SeparateRepair)
                        && report.status == "absent"
                        && report.candidates.is_empty();
                    if !fresh && lease.is_none() {
                        return Err(AppError::Validation(
                            "absent recovery agent has no exact managed lease to resume".into(),
                        ));
                    }
                    if !authority()? {
                        return Err(AppError::Validation(
                            "recovery authority changed during resource inspection".into(),
                        ));
                    }
                    let plan = resume_plan(ctx.store(), candidate)?;
                    dispatching.store(true, Ordering::Release);
                    let outcome = actuator.dispatch_outcome_owned(
                        candidate,
                        &plan,
                        !fresh,
                        ControlOwner {
                            workspace: Some(workspace),
                            cancellation: &cancellation,
                        },
                    )?;
                    if outcome.state == crate::service::engine::DispatchOutcomeState::Ok {
                        Ok(AssessmentDelivery::Delivered)
                    } else {
                        let detail = outcome
                            .payload
                            .get("display")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("managed dispatch refused without diagnostics")
                            .to_string();
                        if outcome
                            .payload
                            .get("reason")
                            .and_then(serde_json::Value::as_str)
                            == Some("handoff-undelivered")
                            && outcome
                                .payload
                                .get("delivery_phase")
                                .and_then(serde_json::Value::as_str)
                                == Some("undelivered")
                        {
                            Ok(AssessmentDelivery::ProvenFailure(detail))
                        } else {
                            Ok(AssessmentDelivery::Uncertain(detail))
                        }
                    }
                }
            }
        })();
        // The sender only exists to end monitoring; a disconnected receiver means
        // authority already failed and its diagnostic takes precedence.
        let _ = finished.send(());
        match watcher.join() {
            Ok(Some(reason)) => Err(AppError::Validation(reason)),
            Ok(None) => result,
            Err(_) => Err(AppError::Storage(
                "recovery authority monitor panicked".into(),
            )),
        }
    });
    match result {
        Ok(delivery) => delivery,
        Err(error) => AssessmentDelivery::Uncertain(error.to_string()),
    }
}

fn message(view: &RecoveryView, operation: &Operation) -> String {
    if operation.effect.is_none() {
        return crate::service::project_recovery::assessment_charter(view);
    }
    let task = if operation.kind == Some(WorkKind::Resume) {
        "Certified repair landed. Refresh source and gate configuration from the current base, reconcile the existing worktree, and resubmit for a fresh verification generation."
    } else {
        "Continue the accepted repair scope in the same recovery lineage. Preserve the worktree and all required coverage and certification."
    };
    format!(
        "PROJECT RECOVERY {} — effect {}. Read `story verifier repair show {} --json` and current story comments. {} Run new and impacted tests, commit, then move the story to verifying as the last action. The central verifier owns submission, the full suite, merge, and cleanup. Do not repeat completed work if this identity was already handled.",
        view.record.id,
        operation.effect.as_deref().unwrap_or_default(),
        view.record.id,
        task
    )
}
