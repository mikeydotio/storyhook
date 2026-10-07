use super::*;
use storyhook::service::project_recovery::{RecoveryView, RepairInput, RepairRefusal, RepairScope};

fn refused(f: &ServiceFixture) -> (RecoveryView, VerificationCandidate) {
    let view = decision::ready(f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = legacy::retain(f, view.clone(), RepairScope::SameStory);
    StoryService::new(&ctx)
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    service
        .admit_repair(
            &candidate,
            "unchanged",
            &RepairInput {
                base: "a".repeat(40),
                head: "b".repeat(40),
                head_tree: "d".repeat(40),
                tree: "f".repeat(40),
            },
        )
        .unwrap();
    (view, candidate)
}

#[test]
fn sh870_retained_settled_refusal_keeps_verifying_and_holds_only_its_exact_submission_once() {
    let f = fixture();
    let (view, candidate) = refused(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let before = service.show(&view.record.id).unwrap();
    let events = f
        .store()
        .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
        .unwrap();
    let applied = service
        .apply_refusal(
            &candidate,
            "unchanged",
            &view.record.id,
            RepairRefusal::UnchangedInput,
        )
        .unwrap()
        .expect("settled refusal needs durable disposition");
    let disposition = applied.state.refusals[0].disposition.as_ref().unwrap();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert_eq!(row.awaiting.as_ref(), Some(&disposition.awaiting));
    assert_eq!(
        applied.state.work, before.state.work,
        "a refusal assigns no new repair work"
    );
    assert_eq!(
        applied.state.decision, before.state.decision,
        "accepted lineage survives"
    );
    let after = f
        .store()
        .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
        .unwrap();
    assert!(!after[events.len()..].iter().any(|e| matches!(
        e.known(),
        Some(storyhook::domain::StoryEvent::StoryStateChanged { .. })
    )));
    assert!(VerificationQueue::new(f.store()).next().unwrap().is_none());
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap(),
        f.store()
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap()
    );
    assert!(
        f.store()
            .read(|tx| tx.block_deliveries(f.project()))
            .unwrap()
            .iter()
            .any(|d| d.story == StoryNo::new(1)
                && d.action == storyhook::store::BlockAction::Interrupt)
    );
    assert_eq!(
        service
            .apply_refusal(
                &candidate,
                "unchanged",
                &view.record.id,
                RepairRefusal::UnchangedInput
            )
            .unwrap()
            .unwrap(),
        applied
    );
    StoryService::new(&ctx).clear_awaiting("SH-1").unwrap();
    service
        .apply_refusal(
            &candidate,
            "unchanged",
            &view.record.id,
            RepairRefusal::UnchangedInput,
        )
        .unwrap();
    assert!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .awaiting
            .is_none(),
        "operator clearing a hold is not undone by replay"
    );
}

#[test]
fn sh870_retained_stale_or_independently_held_refusal_cannot_overwrite_story_state() {
    for change in [
        "awaiting",
        "generation",
        "human-only",
        "no-auto",
        "no-auto-transient",
        "stopped",
        "stop-start",
        "legacy-control",
    ] {
        let f = fixture();
        let (view, candidate) = refused(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        match change {
            "legacy-control" => {
                let mut record = service.show(&view.record.id).unwrap().record;
                record.state["refusals"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("control_revision");
                let expected = record.revision;
                record.revision += 1;
                assert!(
                    f.store()
                        .write(|tx| tx.update_project_recovery(&record, expected))
                        .unwrap()
                );
            }
            "awaiting" => {
                StoryService::new(&ctx)
                    .set_awaiting("SH-1", "operator prerequisite")
                    .unwrap();
            }
            "no-auto-transient" => {
                StoryService::new(&ctx)
                    .set_labels("SH-1", &["no-auto".into()], &[])
                    .unwrap();
                StoryService::new(&ctx)
                    .set_labels("SH-1", &[], &["no-auto".into()])
                    .unwrap();
            }
            "stopped" | "stop-start" => {
                f.store()
                    .write(|tx| {
                        tx.put_verification_enabled(f.project(), false)?;
                        if change == "stop-start" {
                            tx.put_verification_enabled(f.project(), true)?;
                        }
                        Ok(())
                    })
                    .unwrap();
            }
            "generation" => {
                StoryService::new(&ctx)
                    .set_state("SH-1", "in-progress", None, None, None)
                    .unwrap();
                StoryService::new(&ctx)
                    .set_state("SH-1", "verifying", None, None, None)
                    .unwrap();
            }
            label => {
                StoryService::new(&ctx)
                    .set_labels("SH-1", &[label.into()], &[])
                    .unwrap();
            }
        }
        let before = f
            .store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert!(
            service
                .apply_refusal(
                    &candidate,
                    "unchanged",
                    &view.record.id,
                    RepairRefusal::UnchangedInput
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(
            f.store()
                .read(|tx| tx.story(f.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap()
                .snapshot,
            before.snapshot
        );
    }
}

#[test]
fn sh870_retained_refusal_rejects_wrong_record_attempt_reason_and_original_authority() {
    let f = fixture();
    let (view, candidate) = refused(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    assert!(
        service
            .apply_refusal(
                &candidate,
                "other",
                &view.record.id,
                RepairRefusal::UnchangedInput
            )
            .is_err()
    );
    assert!(
        service
            .apply_refusal(
                &candidate,
                "unchanged",
                &view.record.id,
                RepairRefusal::BudgetExhausted
            )
            .is_err()
    );
    let mut other = candidate.clone();
    other.checkout = "/another/repository".into();
    assert!(
        service
            .apply_refusal(
                &other,
                "unchanged",
                &view.record.id,
                RepairRefusal::UnchangedInput
            )
            .is_err()
    );
}
