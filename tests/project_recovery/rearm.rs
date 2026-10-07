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
fn sh870_reservation_readdition_and_new_submission_never_grant_repair_authority() {
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
fn held_repair_work_keeps_its_receipt_and_exact_terminal_disposition() {
    for scope in [RepairScope::SameStory, RepairScope::SeparateStory] {
        let f = fixture();
        let initial = decision::ready(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = legacy::retain(&f, initial.clone(), scope);
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
