use super::*;
use storyhook::service::project_recovery::{RepairScope, WorkKind, WorkStatus};

fn accepted(
    f: &ServiceFixture,
    scope: RepairScope,
) -> storyhook::service::project_recovery::RecoveryView {
    let view = decision::ready(f);
    let ctx = f.ctx();
    ProjectRecoveryService::new(&ctx)
        .decide(&view.record.id, &decision::input(&view, scope))
        .unwrap()
}

#[test]
fn scope_acceptance_enqueues_exactly_one_stable_repair_effect() {
    for scope in [
        RepairScope::SameStory,
        RepairScope::SeparateStory,
        RepairScope::External,
    ] {
        let f = fixture();
        let view = accepted(&f, scope);
        if scope == RepairScope::External {
            assert!(view.state.work.is_empty());
            continue;
        }
        assert_eq!(view.state.work.len(), 1);
        let work = &view.state.work[0];
        let receipt = view.state.decision.as_ref().unwrap();
        assert_eq!(Some(&work.id), receipt.delivery_identity.as_ref());
        assert_eq!(Some(work.story), receipt.repair_story);
        assert_eq!(work.status, WorkStatus::Pending);
        assert_eq!(
            work.kind,
            if scope == RepairScope::SameStory {
                WorkKind::SameStoryRepair
            } else {
                WorkKind::SeparateRepair
            }
        );
        let ctx = f.ctx();
        assert_eq!(
            ProjectRecoveryService::new(&ctx)
                .decide(&view.record.id, &receipt.input)
                .unwrap(),
            view
        );
    }
}

#[test]
fn work_delivery_replays_and_retains_in_flight_identity_across_restart() {
    let f = fixture();
    let view = accepted(&f, RepairScope::SeparateStory);
    let effect = &view.state.work[0];
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let claimed = service
        .claim_work(&view.record.id, &effect.id)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.state.work[0].status, WorkStatus::InFlight);
    assert!(
        service
            .claim_work(&view.record.id, &effect.id)
            .unwrap()
            .is_none()
    );
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let other = storyhook::service::Ctx::new(
        &reopened,
        f.project(),
        f.ctx().cwd().to_path_buf(),
        f.env().clone(),
    );
    let resumed = ProjectRecoveryService::new(&other);
    assert!(
        resumed
            .claim_work(&view.record.id, &effect.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(resumed.show(&view.record.id).unwrap(), claimed);
    let delivered = resumed
        .settle_work(
            &view.record.id,
            &effect.id,
            1,
            AssessmentDelivery::Delivered,
        )
        .unwrap();
    assert_eq!(delivered.state.work[0].status, WorkStatus::Delivered);
    assert_eq!(
        resumed
            .settle_work(
                &view.record.id,
                &effect.id,
                1,
                AssessmentDelivery::Delivered
            )
            .unwrap(),
        delivered
    );
    assert!(
        resumed
            .settle_work(
                &view.record.id,
                &effect.id,
                0,
                AssessmentDelivery::Delivered
            )
            .is_err()
    );
}

#[test]
fn work_proven_failures_are_bounded_and_ambiguous_delivery_is_not_retried() {
    let f = fixture();
    let view = accepted(&f, RepairScope::SameStory);
    let id = &view.state.work[0].id;
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    for epoch in 1..=3 {
        let claimed = service.claim_work(&view.record.id, id).unwrap().unwrap();
        assert_eq!(claimed.state.work[0].epoch, epoch);
        let failed = service
            .settle_work(
                &view.record.id,
                id,
                epoch,
                AssessmentDelivery::ProvenFailure("managed launch proved absent".into()),
            )
            .unwrap();
        assert_eq!(failed.state.work[0].failures, epoch as u8);
    }
    assert!(service.claim_work(&view.record.id, id).unwrap().is_none());
    assert_eq!(
        service.show(&view.record.id).unwrap().state.work[0].hold,
        Some(AssessmentHold::DeliveryExhausted)
    );
    let f = fixture();
    let view = accepted(&f, RepairScope::SeparateStory);
    let id = &view.state.work[0].id;
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service.claim_work(&view.record.id, id).unwrap().unwrap();
    let held = service
        .settle_work(
            &view.record.id,
            id,
            1,
            AssessmentDelivery::Uncertain("provider identity ambiguous".into()),
        )
        .unwrap();
    assert_eq!(
        held.state.work[0].hold,
        Some(AssessmentHold::OwnershipUncertain)
    );
    assert_eq!(held.state.work[0].failures, 0);
    assert!(service.claim_work(&view.record.id, id).unwrap().is_none());
}

#[test]
fn stopped_and_transiently_reserved_repair_targets_cannot_be_dispatched() {
    for stop in [true, false] {
        let f = fixture();
        let view = accepted(&f, RepairScope::SeparateStory);
        let id = &view.state.work[0].id;
        let ctx = f.ctx();
        if stop {
            f.store()
                .write(|tx| tx.put_verification_enabled(f.project(), false))
                .unwrap();
        } else {
            StoryService::new(&ctx)
                .set_labels("SH-2", &["human-only".into()], &[])
                .unwrap();
            StoryService::new(&ctx)
                .set_labels("SH-2", &[], &["human-only".into()])
                .unwrap();
        }
        let service = ProjectRecoveryService::new(&ctx);
        assert!(service.claim_work(&view.record.id, id).unwrap().is_none());
        assert_eq!(
            service.show(&view.record.id).unwrap().state.work[0].hold,
            Some(if stop {
                AssessmentHold::OperatorStop
            } else {
                AssessmentHold::AuthorityChanged
            })
        );
    }
}

#[test]
fn corrupted_effect_cannot_authorize_work_on_another_story() {
    let f = fixture();
    let view = accepted(&f, RepairScope::SeparateStory);
    let mut record = view.record.clone();
    record.revision += 1;
    record.state["work"][0]["story"] = serde_json::json!(1);
    f.store()
        .write(|tx| tx.update_project_recovery(&record, view.record.revision))
        .unwrap();
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    assert!(service.show(&view.record.id).is_err());
    assert!(
        service
            .claim_work(&view.record.id, &view.state.work[0].id)
            .is_err()
    );
}
