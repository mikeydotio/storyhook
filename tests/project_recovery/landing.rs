use super::*;
use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
use storyhook::service::project_recovery::{
    RecoveryView, RepairCompletion, RepairInput, RepairScope,
};

fn repair(f: &ServiceFixture) -> (RecoveryView, VerificationCandidate, RepairInput) {
    let view = decision::ready(f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .decide(
            &view.record.id,
            &decision::input(&view, RepairScope::SameStory),
        )
        .unwrap();
    StoryService::new(&ctx)
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    let input = RepairInput {
        base: "a".repeat(40),
        head: "b".repeat(40),
        head_tree: "e".repeat(40),
        tree: "f".repeat(40),
    };
    service
        .admit_repair(&candidate, "certified-repair", &input)
        .unwrap();
    service
        .complete_repair(
            &candidate,
            "certified-repair",
            &attempts::judgment(&input, RepairCompletion::Certified),
        )
        .unwrap();
    (view, candidate, input)
}

#[test]
fn confirmed_landing_retains_exact_repair_and_merge_event_atomically() {
    let f = fixture();
    let (view, candidate, input) = repair(&f);
    let ctx = f.ctx();
    let queue = VerificationQueue::new(f.store());
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
        ProjectRecoveryService::new(&ctx)
            .show(&view.record.id)
            .unwrap()
            .state
            .landing
            .is_none()
    );
    assert!(
        queue
            .complete_landing(&ctx, &intent, "confirmed exact merge")
            .unwrap()
    );
    let complete = ProjectRecoveryService::new(&ctx)
        .show(&view.record.id)
        .unwrap();
    let receipt = complete
        .state
        .landing
        .as_ref()
        .expect("validated landing must retain recovery receipt");
    assert_eq!(receipt.intent, intent);
    assert_eq!(receipt.attempt, "certified-repair");
    let events = f
        .store()
        .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
        .unwrap();
    assert!(events.iter().any(|e| e.global_seq == receipt.event && matches!(e.known(), Some(storyhook::domain::StoryEvent::StoryPrMerged {url, ..}) if url == &intent.pull_request)));
    assert!(!queue.complete_landing(&ctx, &intent, "duplicate").unwrap());
    assert_eq!(
        ProjectRecoveryService::new(&ctx)
            .show(&view.record.id)
            .unwrap(),
        complete
    );
}

#[test]
fn manual_close_and_green_prose_do_not_prove_repair_landing() {
    let f = fixture();
    let (view, _, _) = repair(&f);
    let ctx = f.ctx();
    StoryService::new(&ctx)
        .comment(
            "SH-1",
            "CENTRAL VERIFICATION GREEN — claimed without a landing receipt",
        )
        .unwrap();
    StoryService::new(&ctx)
        .set_state(
            "SH-1",
            "done",
            Some("operator accepts manual closure"),
            None,
            None,
        )
        .unwrap();
    assert!(
        ProjectRecoveryService::new(&ctx)
            .show(&view.record.id)
            .unwrap()
            .state
            .landing
            .is_none()
    );
}

#[test]
fn landing_that_does_not_match_completed_repair_keeps_pending_authority() {
    let f = fixture();
    let (view, candidate, input) = repair(&f);
    let ctx = f.ctx();
    let queue = VerificationQueue::new(f.store());
    let cert = VerifiedSubmission {
        head: input.head,
        tree: "1".repeat(40),
        gate: "make test".into(),
    };
    let LandingAdmission::Admitted(intent) = queue.begin_landing(&ctx, &candidate, &cert).unwrap()
    else {
        panic!("landing admission")
    };
    assert!(queue.complete_landing(&ctx, &intent, "wrong tree").is_err());
    assert_eq!(
        f.store().read(|tx| tx.landing_intents()).unwrap(),
        vec![intent]
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "verifying"
    );
    assert!(
        ProjectRecoveryService::new(&ctx)
            .show(&view.record.id)
            .unwrap()
            .state
            .landing
            .is_none()
    );
}

#[test]
fn retained_landing_cannot_substitute_an_unrelated_completion_event() {
    let f = fixture();
    let (view, candidate, input) = repair(&f);
    let ctx = f.ctx();
    let queue = VerificationQueue::new(f.store());
    let cert = VerifiedSubmission {
        head: input.head,
        tree: input.tree,
        gate: "make test".into(),
    };
    let LandingAdmission::Admitted(intent) = queue.begin_landing(&ctx, &candidate, &cert).unwrap()
    else {
        panic!("landing admission")
    };
    queue
        .complete_landing(&ctx, &intent, "confirmed merge")
        .unwrap();
    f.store()
        .write(|tx| {
            let mut record = tx.project_recoveries(f.project())?.remove(0);
            let revision = record.revision;
            // A state transition after admission is not the confirmed PR merge event.
            let event = tx
                .events_for(f.project(), StoryNo::new(1))?
                .into_iter()
                .rev()
                .find(|e| {
                    matches!(
                        e.known(),
                        Some(storyhook::domain::StoryEvent::StoryStateChanged { .. })
                    )
                })
                .unwrap()
                .global_seq;
            record.state["landing"]["event"] = serde_json::to_value(event).unwrap();
            record.revision += 1;
            assert!(tx.update_project_recovery(&record, revision)?);
            Ok(())
        })
        .unwrap();
    assert!(
        ProjectRecoveryService::new(&ctx)
            .show(&view.record.id)
            .is_err()
    );
}
