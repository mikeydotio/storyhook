//! Policy release must resume the retained assessment, never the unjudged gate.
use super::*;
use std::sync::atomic::AtomicBool;
use storyhook::daemon::{project_recovery::process_one, verification::VerificationActivity};
use storyhook::service::project_recovery::{RecoveryView, RepairScope, WorkStatus};

pub(super) fn reserved(f: &ServiceFixture, labels: &[&str], stopped: bool) -> RecoveryView {
    let candidate = submitted(f, "held at enrollment");
    let ctx = f.ctx();
    let initial_labels = if labels.contains(&"human-only") {
        vec!["no-auto"]
    } else {
        labels.to_vec()
    };
    StoryService::new(&ctx)
        .set_labels(
            "SH-1",
            &initial_labels
                .iter()
                .map(|label| (*label).into())
                .collect::<Vec<_>>(),
            &[],
        )
        .unwrap();
    f.store()
        .write(|tx| tx.put_verification_enabled(f.project(), !stopped))
        .unwrap();
    let service = ProjectRecoveryService::new(&ctx);
    let mut view = service
        .observe(&candidate, &fault(), "held")
        .unwrap()
        .unwrap();
    if labels.contains(&"human-only") {
        // Compatibility input: current observe refuses new faults under human-only,
        // but persisted v1 policy holds must use the same release evaluator.
        let remove = if labels.contains(&"no-auto") {
            vec![]
        } else {
            vec!["no-auto".into()]
        };
        StoryService::new(&ctx)
            .set_labels(
                "SH-1",
                &labels
                    .iter()
                    .map(|label| (*label).into())
                    .collect::<Vec<_>>(),
                &remove,
            )
            .unwrap();
        view.state.subjects[0].label_revision = f
            .store()
            .read(|tx| {
                Ok(tx
                    .events_for(f.project(), StoryNo::new(1))?
                    .iter()
                    .rev()
                    .find(|event| {
                        matches!(
                            event.known(),
                            Some(storyhook::domain::StoryEvent::StoryLabelsSet { .. })
                        )
                    })
                    .map(|event| event.global_seq))
            })
            .unwrap();
        let revision = view.record.revision;
        view.record.revision += 1;
        view.record.state = serde_json::to_value(&view.state).unwrap();
        assert!(
            f.store()
                .write(|tx| tx.update_project_recovery(&view.record, revision))
                .unwrap()
        );
        view = service.show(&view.record.id).unwrap();
    }
    view
}

pub(super) fn release(f: &ServiceFixture) {
    StoryService::new(&f.ctx())
        .set_labels("SH-1", &[], &["no-auto".into(), "human-only".into()])
        .unwrap();
    f.store()
        .write(|tx| tx.put_verification_enabled(f.project(), true))
        .unwrap();
}

fn status(f: &ServiceFixture) -> String {
    VerificationActivity::new()
        .status(&f.ctx())
        .unwrap()
        .project_recoveries[0]
        .next_action
        .clone()
}

fn unchanged(f: &ServiceFixture, view: &RecoveryView) {
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    assert!(!service.policy_rearm_ready(&view.record.id, None).unwrap());
    assert!(!service.rearm_policy_hold(&view.record.id, None).unwrap());
    assert_eq!(service.show(&view.record.id).unwrap(), *view);
}

#[test]
fn each_initial_policy_releases_only_after_all_controls_clear() {
    for (labels, stopped) in [
        (vec!["no-auto"], false),
        (vec!["human-only"], false),
        (vec!["no-auto", "human-only"], false),
        (vec![], true),
        (vec!["no-auto", "human-only"], true),
    ] {
        let f = fixture();
        let view = reserved(&f, &labels, stopped);
        unchanged(&f, &view);
        assert!(status(&f).contains(if stopped { "stop" } else { "reservation" }));
        if labels.len() == 2 {
            StoryService::new(&f.ctx())
                .set_labels("SH-1", &[], &["no-auto".into()])
                .unwrap();
            unchanged(&f, &view);
        }
        release(&f);
        assert!(status(&f).contains("wait for managed reconciliation"));
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
        let delivered = ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .unwrap();
        assert_eq!(
            delivered.state.assessment.status,
            AssessmentStatus::Delivered
        );
        assert_eq!(delivered.observations, view.observations);
        assert_eq!(
            delivered.state.subjects[0].candidate,
            view.state.subjects[0].candidate
        );
        assert!(!status(&f).contains("reservation prevents"));
    }
}

#[test]
fn reservation_readdition_and_new_submission_never_renew_old_authority() {
    for change in [
        "readd",
        "replace-label",
        "new-generation",
        "state",
        "block-clear",
        "awaiting",
        "dependency",
    ] {
        let f = fixture();
        let view = reserved(&f, &["no-auto"], false);
        release(&f);
        let ctx = f.ctx();
        let stories = StoryService::new(&ctx);
        match change {
            "readd" | "replace-label" => {
                let label = if change == "readd" {
                    "no-auto"
                } else {
                    "human-only"
                };
                stories.set_labels("SH-1", &[label.into()], &[]).unwrap();
                stories.set_labels("SH-1", &[], &[label.into()]).unwrap();
            }
            "new-generation" => {
                stories
                    .set_state("SH-1", "in-progress", None, None, None)
                    .unwrap();
                stories
                    .set_state("SH-1", "verifying", None, None, None)
                    .unwrap();
            }
            "state" => {
                stories.set_state("SH-1", "todo", None, None, None).unwrap();
            }
            "block-clear" | "awaiting" => {
                stories
                    .set_awaiting("SH-1", "independent operator hold")
                    .unwrap();
                if change == "block-clear" {
                    stories.clear_awaiting("SH-1").unwrap();
                }
            }
            "dependency" => {
                let other = stories
                    .create(&NewStoryInput {
                        title: "independent dependency".into(),
                        ..Default::default()
                    })
                    .unwrap();
                storyhook::service::RelationService::new(&ctx)
                    .block_on("SH-1", &[other.id], None)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        unchanged(&f, &view);
        assert!(!status(&f).contains("reservation prevents"), "{change}");
        assert!(
            !status(&f).contains("wait for managed reconciliation"),
            "{change}"
        );
        let actuator = worker::helper(&f, r#"{"ok":true}"#);
        assert!(
            !process_one(
                f.store(),
                f.env(),
                &actuator,
                &VerificationActivity::new(),
                &AtomicBool::new(false)
            )
            .unwrap(),
            "{change}"
        );
        assert!(
            !view.state.subjects[0]
                .candidate
                .checkout
                .join("recovery-calls")
                .exists()
        );
    }
}

#[test]
fn recheck_catches_readded_reservation_between_selection_and_transaction() {
    let f = fixture();
    let view = reserved(&f, &["no-auto"], false);
    release(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    assert!(service.policy_rearm_ready(&view.record.id, None).unwrap());
    StoryService::new(&ctx)
        .set_labels("SH-1", &["no-auto".into()], &[])
        .unwrap();
    unchanged(&f, &view);
    release(&f);
    unchanged(&f, &view);
}

#[test]
fn restart_and_workspace_owner_preserve_single_return_and_delivery() {
    let f = fixture();
    let view = reserved(&f, &["no-auto"], false);
    release(&f);
    let candidate = &view.state.subjects[0].candidate;
    let locks = candidate.checkout.join(".git/storyhook/workspace-locks");
    std::fs::create_dir_all(&locks).unwrap();
    let lock = std::fs::File::create(locks.join("SH-1.lock")).unwrap();
    fs4::FileExt::lock_exclusive(&lock).unwrap();
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let actuator = worker::helper(&f, r#"{"ok":true}"#);
    let activity = VerificationActivity::new();
    let stop = AtomicBool::new(false);
    assert!(!process_one(&reopened, f.env(), &actuator, &activity, &stop).unwrap());
    assert_eq!(
        ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .unwrap(),
        view
    );
    drop(lock);
    assert!(process_one(&reopened, f.env(), &actuator, &activity, &stop).unwrap());
    assert!(!process_one(f.store(), f.env(), &actuator, &activity, &stop).unwrap());
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let delivered = service.show(&view.record.id).unwrap();
    assert_eq!(delivered.state.assessment.epoch, 1);
    assert_eq!(
        std::fs::read_to_string(candidate.checkout.join("recovery-calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(
        service
            .settle_assessment(
                &view.record.id,
                &delivered.state.assessment.dispatch_identity,
                0,
                AssessmentDelivery::Delivered
            )
            .is_err()
    );
}

#[test]
fn policy_masked_delivery_results_and_terminal_holds_never_replay() {
    for reserved in [false, true] {
        for result in [
            AssessmentDelivery::Delivered,
            AssessmentDelivery::Uncertain("receipt lost".into()),
            AssessmentDelivery::ProvenFailure("handoff absent".into()),
        ] {
            let f = fixture();
            let candidate = submitted(&f, "masked delivery");
            let ctx = f.ctx();
            let service = ProjectRecoveryService::new(&ctx);
            let view = service
                .observe(&candidate, &fault(), "masked")
                .unwrap()
                .unwrap();
            let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
            if reserved {
                StoryService::new(&ctx)
                    .set_labels("SH-1", &["no-auto".into()], &[])
                    .unwrap();
            } else {
                f.store()
                    .write(|tx| tx.put_verification_enabled(f.project(), false))
                    .unwrap();
            }
            let held = service
                .settle_assessment(
                    &view.record.id,
                    &claimed.state.assessment.dispatch_identity,
                    1,
                    result.clone(),
                )
                .unwrap();
            assert_eq!(
                held.state.assessment.hold,
                Some(if reserved {
                    AssessmentHold::ReservedLabel
                } else {
                    AssessmentHold::OperatorStop
                })
            );
            assert_eq!(held.state.assessment.last_result, Some(result));
            release(&f);
            unchanged(&f, &held);
            assert!(!status(&f).contains("reservation prevents"));
            StoryService::new(&ctx).clear_awaiting("SH-1").unwrap();
            unchanged(&f, &held);
        }
    }
}

#[test]
fn held_repair_work_keeps_its_receipt_and_exact_terminal_disposition() {
    for scope in [RepairScope::SameStory, RepairScope::SeparateStory] {
        let f = fixture();
        let initial = decision::ready(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = service
            .decide(&initial.record.id, &decision::input(&initial, scope))
            .unwrap();
        let effect = &view.state.work[0].id;
        let claimed = service
            .claim_work(&view.record.id, effect)
            .unwrap()
            .unwrap();
        f.store()
            .write(|tx| tx.put_verification_enabled(f.project(), false))
            .unwrap();
        let held = service
            .settle_work(
                &view.record.id,
                effect,
                claimed.state.work[0].epoch,
                AssessmentDelivery::Delivered,
            )
            .unwrap();
        assert_eq!(held.state.work[0].status, WorkStatus::Held);
        release(&f);
        assert!(
            !service
                .policy_rearm_ready(&view.record.id, Some(effect))
                .unwrap()
        );
        assert!(
            !service
                .rearm_policy_hold(&view.record.id, Some(effect))
                .unwrap()
        );
        assert_eq!(service.show(&view.record.id).unwrap(), held);
        assert!(status(&f).contains("ownership reconciliation"));
    }
}

#[test]
fn removing_initial_reservation_delivers_once_and_preserves_fault() {
    let f = fixture();
    let candidate = submitted(&f, "initial reservation");
    let ctx = f.ctx();
    let stories = StoryService::new(&ctx);
    stories
        .set_labels("SH-1", &["no-auto".into()], &[])
        .unwrap();
    let service = ProjectRecoveryService::new(&ctx);
    let initial = service
        .observe(&candidate, &fault(), "held-fault")
        .unwrap()
        .unwrap();
    assert!(!initial.state.subjects[0].returned);
    stories
        .set_labels("SH-1", &[], &["no-auto".into()])
        .unwrap();
    let actuator = worker::helper(&f, r#"{"ok":true}"#);
    let activity = VerificationActivity::new();
    let stop = AtomicBool::new(false);
    assert!(process_one(f.store(), f.env(), &actuator, &activity, &stop).unwrap());
    let delivered = service.show(&initial.record.id).unwrap();
    assert_eq!(
        delivered.state.assessment.status,
        AssessmentStatus::Delivered
    );
    assert_eq!(delivered.state.assessment.epoch, 1);
    assert_eq!(
        delivered.state.assessment.dispatch_identity,
        initial.state.assessment.dispatch_identity
    );
    assert_eq!(delivered.observations, initial.observations);
    assert_eq!(delivered.state.subjects[0].candidate, candidate);
    assert!(delivered.state.subjects[0].returned);
    assert!(delivered.state.work.is_empty());
    assert!(
        VerificationQueue::new(f.store())
            .ordered_for(f.project())
            .unwrap()
            .is_empty()
    );
    assert!(!process_one(f.store(), f.env(), &actuator, &activity, &stop).unwrap());
    assert_eq!(service.show(&initial.record.id).unwrap(), delivered);
    assert_eq!(
        std::fs::read_to_string(candidate.checkout.join("recovery-calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let mut input = decision::input(&delivered, RepairScope::SeparateStory);
    input.evidence = vec!["attempt:held-fault".into()];
    let accepted = service.decide(&initial.record.id, &input).unwrap();
    assert_eq!(
        service.decide(&initial.record.id, &input).unwrap(),
        accepted
    );
    assert_eq!(accepted.state.work.len(), 1);
    assert_eq!(
        accepted.state.assessment.generation,
        initial.state.assessment.generation
    );
    assert_eq!(accepted.observations, initial.observations);
}
