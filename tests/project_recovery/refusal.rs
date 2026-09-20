use super::*;
use storyhook::service::project_recovery::{RecoveryView, RepairInput, RepairRefusal, RepairScope};

fn refused(f: &ServiceFixture) -> (RecoveryView, VerificationCandidate) {
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
fn settled_refusal_returns_and_holds_only_its_exact_submission_once() {
    let f = fixture();
    let (view, candidate) = refused(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
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
    assert_eq!(row.state, "in-progress");
    assert_eq!(row.awaiting.as_ref(), Some(&disposition.awaiting));
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
fn stale_or_independently_held_refusal_cannot_overwrite_story_state() {
    for change in [
        "awaiting",
        "generation",
        "human-only",
        "no-auto",
        "no-auto-transient",
        "stopped",
    ] {
        let f = fixture();
        let (view, candidate) = refused(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        match change {
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
            "stopped" => {
                f.store()
                    .write(|tx| tx.put_verification_enabled(f.project(), false))
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
fn refusal_rejects_wrong_record_attempt_reason_and_original_authority() {
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
