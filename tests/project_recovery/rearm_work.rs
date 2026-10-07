//! Old policy-only work can rearm, but existing delivery and hold evidence wins.
use super::*;
use std::sync::atomic::AtomicBool;
use storyhook::daemon::{project_recovery::process_one, verification::VerificationActivity};
use storyhook::service::project_recovery::{RecoveryView, RepairScope, WorkKind, WorkStatus};

fn retained_policy_work(
    f: &ServiceFixture,
    mut view: RecoveryView,
    index: usize,
    failures: u8,
) -> RecoveryView {
    // Isolated persisted-v1 input, not a substitute for the production claim/delivery.
    // No terminal disposition exists in this legacy policy-only effect.
    let work = &mut view.state.work[index];
    work.status = WorkStatus::Held;
    work.hold = Some(AssessmentHold::OperatorStop);
    work.failures = failures;
    work.epoch = failures.into();
    if failures > 0 {
        work.started_at = Some(view.state.created_at.clone());
        work.last_result = Some(AssessmentDelivery::ProvenFailure("proven absent".into()));
    }
    let revision = view.record.revision;
    view.record.revision += 1;
    view.record.state = serde_json::to_value(&view.state).unwrap();
    assert!(
        f.store()
            .write(|tx| tx.update_project_recovery(&view.record, revision))
            .unwrap()
    );
    ProjectRecoveryService::new(&f.ctx())
        .show(&view.record.id)
        .unwrap()
}

#[test]
fn sh870_retained_legacy_undelivered_work_rearms_without_resetting_budget_or_identity() {
    for failures in [0, 1, 2, 3] {
        let f = fixture();
        let initial = decision::ready(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let accepted = legacy::retain(&f, initial.clone(), RepairScope::SameStory);
        let held = retained_policy_work(&f, accepted, 0, failures);
        let effect = &held.state.work[0];
        let actuator = worker::helper(&f, r#"{"ok":true}"#);
        let activity = VerificationActivity::new();
        let stop = AtomicBool::new(false);
        assert_eq!(
            process_one(f.store(), f.env(), &actuator, &activity, &stop).unwrap(),
            failures < 3
        );
        let final_view = service.show(&held.record.id).unwrap();
        if failures < 3 {
            let delivered = &final_view.state.work[0];
            assert_eq!(delivered.status, WorkStatus::Delivered);
            assert_eq!(delivered.id, effect.id);
            assert_eq!(delivered.failures, failures);
            assert_eq!(delivered.epoch, u32::from(failures) + 1);
            assert!(
                service
                    .settle_work(
                        &held.record.id,
                        &effect.id,
                        effect.epoch,
                        AssessmentDelivery::Delivered
                    )
                    .is_err()
            );
        } else {
            assert_eq!(final_view, held);
        }
        assert_eq!(final_view.observations, held.observations);
        assert_eq!(final_view.state.decision, held.state.decision);
        assert!(!process_one(f.store(), f.env(), &actuator, &activity, &stop).unwrap());
    }
}

#[test]
fn sh870_retained_inactive_recovery_rearms_only_undelivered_resume_work() {
    let f = fixture();
    let accepted = resume::decided(&f);
    resume::land(&f, &accepted);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let released = service.reconcile_landing(&accepted.record.id).unwrap();
    let index = released
        .state
        .work
        .iter()
        .position(|work| work.kind == WorkKind::Resume)
        .unwrap();
    let held = retained_policy_work(&f, released, index, 0);
    let effect = &held.state.work[index];
    assert!(!held.record.active);
    let actuator = worker::helper(&f, r#"{"ok":true}"#);
    assert!(
        process_one(
            f.store(),
            f.env(),
            &actuator,
            &VerificationActivity::new(),
            &AtomicBool::new(false)
        )
        .unwrap()
    );
    let delivered = service.show(&held.record.id).unwrap();
    assert_eq!(delivered.state.work[index].status, WorkStatus::Delivered);
    assert_eq!(delivered.state.work[index].id, effect.id);
    assert_eq!(
        delivered.state.work[index].release_event,
        effect.release_event
    );
    assert_eq!(delivered.state.landing, held.state.landing);
}

#[test]
fn sh870_retained_stopped_resume_with_terminal_disposition_stays_held_after_start() {
    let f = fixture();
    let accepted = resume::decided(&f);
    resume::land(&f, &accepted);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let released = service.reconcile_landing(&accepted.record.id).unwrap();
    let index = released
        .state
        .work
        .iter()
        .position(|work| work.kind == WorkKind::Resume)
        .unwrap();
    let effect = &released.state.work[index].id;
    f.store()
        .write(|tx| tx.put_verification_enabled(f.project(), false))
        .unwrap();
    assert!(
        service
            .claim_work(&released.record.id, effect)
            .unwrap()
            .is_none()
    );
    let held = service.show(&released.record.id).unwrap();
    assert!(held.state.work[index].disposition.is_some());
    f.store()
        .write(|tx| tx.put_verification_enabled(f.project(), true))
        .unwrap();
    assert!(
        !service
            .rearm_policy_hold(&released.record.id, Some(effect))
            .unwrap()
    );
    assert_eq!(service.show(&released.record.id).unwrap(), held);
}
