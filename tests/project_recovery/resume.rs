use super::*;
use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
use storyhook::service::project_recovery::{
    RecoveryView, RepairCompletion, RepairInput, RepairScope, WorkKind,
};

fn decided(f: &ServiceFixture) -> RecoveryView {
    let initial = decision::ready(f);
    let ctx = f.ctx();
    ProjectRecoveryService::new(&ctx)
        .decide(
            &initial.record.id,
            &decision::input(&initial, RepairScope::SeparateStory),
        )
        .unwrap()
}

fn land(f: &ServiceFixture, view: &RecoveryView) {
    let ctx = f.ctx();
    let repair = view
        .state
        .decision
        .as_ref()
        .unwrap()
        .repair_story
        .unwrap()
        .to_id("SH");
    PrLinkService::new(&ctx)
        .link(&repair, "https://github.com/acme/widgets/pull/2", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&repair, "verifying", None, None, None)
        .unwrap();
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    let service = ProjectRecoveryService::new(&ctx);
    let input = RepairInput {
        base: "a".repeat(40),
        head: "b".repeat(40),
        head_tree: "e".repeat(40),
        tree: "f".repeat(40),
    };
    service
        .admit_repair(&candidate, "landed-repair", &input)
        .unwrap();
    service
        .complete_repair(
            &candidate,
            "landed-repair",
            &attempts::judgment(&input, RepairCompletion::Certified),
        )
        .unwrap();
    let cert = VerifiedSubmission {
        head: input.head,
        tree: input.tree,
        gate: "make test".into(),
    };
    let LandingAdmission::Admitted(intent) = queue.begin_landing(&ctx, &candidate, &cert).unwrap()
    else {
        panic!("landing admission")
    };
    assert!(
        queue
            .complete_landing(&ctx, &intent, "confirmed certified merge")
            .unwrap()
    );
}

#[test]
fn confirmed_landing_retires_fault_and_resumes_exact_submission_once() {
    let f = fixture();
    let view = decided(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    assert_eq!(service.reconcile_landing(&view.record.id).unwrap(), view);
    land(&f, &view);
    assert!(!service.show(&view.record.id).unwrap().record.active);
    let resumed = service.reconcile_landing(&view.record.id).unwrap();
    let work = resumed
        .state
        .work
        .iter()
        .find(|w| w.kind == WorkKind::Resume)
        .expect("durable resume");
    assert_eq!(work.story, StoryNo::new(1));
    assert!(
        f.store()
            .read(|tx| tx.story(f.project(), work.story))
            .unwrap()
            .unwrap()
            .awaiting
            .is_none()
    );
    assert!(
        f.store()
            .read(|tx| tx.block_deliveries(f.project()))
            .unwrap()
            .iter()
            .all(|d| d.action != storyhook::store::BlockAction::Resume)
    );
    assert_eq!(service.reconcile_landing(&view.record.id).unwrap(), resumed);
    assert!(
        service
            .claim_work(&view.record.id, &work.id)
            .unwrap()
            .is_some()
    );
    service
        .settle_work(&view.record.id, &work.id, 1, AssessmentDelivery::Delivered)
        .unwrap();
    let next = submitted(&f, "later independent fault");
    let later = service
        .observe(&next, &fault(), "new-fault")
        .unwrap()
        .unwrap();
    assert_ne!(later.record.id, view.record.id);
}

#[test]
fn unrelated_dependency_delays_owned_hold_release_until_it_clears() {
    let f = fixture();
    let view = decided(&f);
    let ctx = f.ctx();
    let other = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "independent dependency".into(),
            ..Default::default()
        })
        .unwrap();
    storyhook::service::RelationService::new(&ctx)
        .block_on("SH-1", std::slice::from_ref(&other.id), None)
        .unwrap();
    land(&f, &view);
    let service = ProjectRecoveryService::new(&ctx);
    let held = service.reconcile_landing(&view.record.id).unwrap();
    assert!(held.state.work.iter().all(|w| w.kind != WorkKind::Resume));
    assert!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .awaiting
            .is_some()
    );
    StoryService::new(&ctx)
        .set_state(&other.id, "done", Some("dependency complete"), None, None)
        .unwrap();
    let released = service.reconcile_landing(&view.record.id).unwrap();
    assert_eq!(
        released
            .state
            .work
            .iter()
            .filter(|w| w.kind == WorkKind::Resume)
            .count(),
        1
    );
}

#[test]
fn replaced_even_identical_hold_and_changed_state_are_not_recovery_authority() {
    for mutation in ["replace", "state", "clear"] {
        let f = fixture();
        let view = decided(&f);
        let ctx = f.ctx();
        let stories = StoryService::new(&ctx);
        match mutation {
            "replace" => {
                stories.clear_awaiting("SH-1").unwrap();
                stories
                    .set_awaiting(
                        "SH-1",
                        &view.state.decision.as_ref().unwrap().dependency_holds[0].awaiting,
                    )
                    .unwrap();
            }
            "state" => {
                stories.set_state("SH-1", "todo", None, None, None).unwrap();
            }
            _ => {
                stories.clear_awaiting("SH-1").unwrap();
            }
        }
        land(&f, &view);
        let after = ProjectRecoveryService::new(&ctx)
            .reconcile_landing(&view.record.id)
            .unwrap();
        assert!(
            after.state.work.iter().all(|w| w.kind != WorkKind::Resume),
            "{mutation}"
        );
    }
}

#[test]
fn later_unrelated_unblock_keeps_ordinary_resume_and_stale_recovery_cannot_claim() {
    let f = fixture();
    let view = decided(&f);
    land(&f, &view);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let resumed = service.reconcile_landing(&view.record.id).unwrap();
    let effect = resumed
        .state
        .work
        .iter()
        .find(|w| w.kind == WorkKind::Resume)
        .unwrap();
    StoryService::new(&ctx)
        .set_awaiting("SH-1", "independent operator hold")
        .unwrap();
    StoryService::new(&ctx).clear_awaiting("SH-1").unwrap();
    assert!(
        f.store()
            .read(|tx| tx.block_deliveries(f.project()))
            .unwrap()
            .iter()
            .any(|d| d.action == storyhook::store::BlockAction::Resume)
    );
    assert!(
        service
            .claim_work(&view.record.id, &effect.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn resume_intent_rolls_back_with_failed_story_append() {
    let f = fixture();
    let view = decided(&f);
    land(&f, &view);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let before = service.show(&view.record.id).unwrap();
    let connection = rusqlite::Connection::open(f.store().path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_recovery_resume BEFORE UPDATE ON project_recoveries BEGIN SELECT RAISE(ABORT, 'injected resume failure'); END;").unwrap();
    assert!(service.reconcile_landing(&view.record.id).is_err());
    assert_eq!(service.show(&view.record.id).unwrap(), before);
    assert!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .awaiting
            .is_some()
    );
    connection
        .execute_batch("DROP TRIGGER refuse_recovery_resume;")
        .unwrap();
    assert!(
        service
            .reconcile_landing(&view.record.id)
            .unwrap()
            .state
            .work
            .iter()
            .any(|w| w.kind == WorkKind::Resume)
    );
}

#[test]
fn stop_and_transient_reservation_preserve_owned_holds_after_landing() {
    for stop in [true, false] {
        let f = fixture();
        let view = decided(&f);
        land(&f, &view);
        let ctx = f.ctx();
        if stop {
            f.store()
                .write(|tx| tx.put_verification_enabled(f.project(), false))
                .unwrap();
        } else {
            StoryService::new(&ctx)
                .set_labels("SH-1", &["no-auto".into()], &[])
                .unwrap();
            StoryService::new(&ctx)
                .set_labels("SH-1", &[], &["no-auto".into()])
                .unwrap();
        }
        let service = ProjectRecoveryService::new(&ctx);
        let held = service.reconcile_landing(&view.record.id).unwrap();
        assert!(held.state.work.iter().all(|w| w.kind != WorkKind::Resume));
        assert!(
            f.store()
                .read(|tx| tx.story(f.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap()
                .awaiting
                .is_some()
        );
        if stop {
            f.store()
                .write(|tx| tx.put_verification_enabled(f.project(), true))
                .unwrap();
            assert!(
                service
                    .reconcile_landing(&view.record.id)
                    .unwrap()
                    .state
                    .work
                    .iter()
                    .any(|w| w.kind == WorkKind::Resume)
            );
        }
    }
}
