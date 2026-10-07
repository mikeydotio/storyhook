use super::*;
use storyhook::daemon::verification::{VerificationActivity, status::VerifierStatus};
use storyhook::service::project_recovery::{RecoveryStatus, RecoveryView, RepairScope, WorkKind};

#[test]
fn sh870_retained_shared_status_exposes_repair_ownership_and_legacy_payloads_default_empty() {
    let f = fixture();
    let view = decision::ready(&f);
    let ctx = f.ctx();
    let accepted = legacy::retain(&f, view.clone(), RepairScope::SeparateStory);
    let activity = VerificationActivity::new();
    let status = activity.status(&ctx).unwrap();
    let json = serde_json::to_value(&status).unwrap();
    let recovery = &json["project_recoveries"][0];
    assert_eq!(recovery["id"], accepted.record.id);
    assert_eq!(recovery["fault"], "missing-certification");
    assert_eq!(recovery["affected_stories"], serde_json::json!(["SH-1"]));
    assert_eq!(recovery["assessment_owner"], "SH-1");
    assert_eq!(recovery["repair_story"], "SH-2");
    assert_eq!(recovery["phase"], "repair-pending");
    assert_eq!(recovery["completed_attempts"], 0);
    assert_eq!(recovery["attempt_limit"], 3);
    assert!(recovery["next_action"].as_str().unwrap().contains("SH-2"));
    assert!(status.incident.is_none());
    assert!(status.render_human().contains("repair-pending"));
    let mut old = json;
    old.as_object_mut().unwrap().remove("project_recoveries");
    let decoded: VerifierStatus = serde_json::from_value(old).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap()["project_recoveries"],
        serde_json::json!([])
    );
}

#[test]
fn reserved_generation_is_visible_as_recovery_hold_not_infrastructure() {
    let f = fixture();
    let candidate = submitted(&f, "reserved");
    let ctx = f.ctx();
    StoryService::new(&ctx)
        .set_labels("SH-1", &["no-auto".into()], &[])
        .unwrap();
    ProjectRecoveryService::new(&ctx)
        .observe(&candidate, &fault(), "reserved")
        .unwrap();
    let status = VerificationActivity::new().status(&ctx).unwrap();
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["project_recoveries"][0]["phase"], "held");
    assert!(
        json["project_recoveries"][0]["next_action"]
            .as_str()
            .unwrap()
            .contains("no-auto")
    );
    assert!(status.verifying.is_empty());
    assert!(status.incident.is_none());
}

/// The status row of one recovery, or `None` once the snapshot leaves it out.
fn current(f: &ServiceFixture, id: &str) -> Option<RecoveryStatus> {
    let ctx = f.ctx();
    VerificationActivity::new()
        .status(&ctx)
        .unwrap()
        .project_recoveries
        .into_iter()
        .find(|row| row.id == id)
}

fn phase(f: &ServiceFixture, id: &str) -> Option<String> {
    current(f, id).map(|row| row.phase)
}

/// SH-1 and SH-2 hit the same fault; SH-3 is the separate repair.
fn two_affected(f: &ServiceFixture) -> RecoveryView {
    let first = decision::ready(f);
    let second = submitted(f, "second affected submission");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .observe(&second, &fault(), "second-attempt")
        .unwrap()
        .unwrap();
    let joined = service.show(&first.record.id).unwrap();
    assert_eq!(joined.state.subjects.len(), 2);
    legacy::retain(f, joined.clone(), RepairScope::SeparateStory)
}

fn deliver_resumes(f: &ServiceFixture, id: &str) {
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let effects: Vec<String> = service
        .show(id)
        .unwrap()
        .state
        .work
        .into_iter()
        .filter(|w| w.kind == WorkKind::Resume)
        .map(|w| w.id)
        .collect();
    assert!(!effects.is_empty());
    for effect in effects {
        let claimed = service.claim_work(id, &effect).unwrap().unwrap();
        let epoch = claimed
            .state
            .work
            .iter()
            .find(|w| w.id == effect)
            .unwrap()
            .epoch;
        service
            .settle_work(id, &effect, epoch, AssessmentDelivery::Delivered)
            .unwrap();
    }
}

#[test]
fn sh870_retained_landed_recovery_names_only_owed_stories_and_leaves_status_once_none_owes() {
    for discharge in ["resubmit", "drop"] {
        let f = fixture();
        let view = two_affected(&f);
        let id = view.record.id.clone();
        assert_eq!(
            view.state.decision.as_ref().unwrap().dependency_holds.len(),
            2
        );
        resume::land(&f, &view);
        let held = current(&f, &id).expect("owned holds remain after landing");
        assert_eq!(held.phase, "resume-held", "{discharge}");
        assert!(held.next_action.contains("SH-1, SH-2"), "{held:?}");

        let ctx = f.ctx();
        ProjectRecoveryService::new(&ctx)
            .reconcile_landing(&id)
            .unwrap();
        assert_eq!(phase(&f, &id).as_deref(), Some("resume-pending"));

        deliver_resumes(&f, &id);
        let owed = current(&f, &id).expect("resumed stories still owe a generation");
        assert_eq!(owed.phase, "landed");
        assert!(owed.next_action.contains("SH-1, SH-2"), "{owed:?}");
        assert!(!owed.next_action.contains("Affected agents"), "{owed:?}");

        let stories = StoryService::new(&ctx);
        stories
            .set_state("SH-1", "verifying", None, None, None)
            .unwrap();
        let one = current(&f, &id).expect("SH-2 still owes a generation");
        assert_eq!(one.phase, "landed");
        assert!(one.next_action.contains("SH-2"), "{one:?}");
        assert!(
            !one.next_action.contains("SH-1"),
            "SH-1 already resubmitted: {one:?}"
        );

        stories.set_state("SH-2", "todo", None, None, None).unwrap();
        let parked = current(&f, &id).expect("a parked story still owes a generation");
        assert!(parked.next_action.contains("SH-2"), "{parked:?}");

        match discharge {
            "resubmit" => {
                stories
                    .set_state("SH-2", "verifying", None, None, None)
                    .unwrap();
            }
            _ => {
                stories
                    .set_state("SH-2", "dropped", Some("abandoned"), None, None)
                    .unwrap();
            }
        }
        assert_eq!(current(&f, &id), None, "{discharge}");
        if discharge == "drop" {
            stories.reopen("SH-2").unwrap();
            assert_eq!(current(&f, &id), None, "a reopen cannot revive it");
        }
        let retained = f
            .store()
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap();
        assert!(retained.iter().any(|r| r.id == id && !r.active));
    }
}

#[test]
fn sh870_retained_a_current_recovery_hold_keeps_the_row_after_its_story_resubmits() {
    let f = fixture();
    let view = resume::decided(&f);
    let id = view.record.id.clone();
    resume::land(&f, &view);
    let ctx = f.ctx();
    let stories = StoryService::new(&ctx);
    stories
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    let held = current(&f, &id).expect("the recovery hold is still on SH-1");
    assert_eq!(held.phase, "resume-held");
    assert!(held.next_action.contains("SH-1"), "{held:?}");
    stories.clear_awaiting("SH-1").unwrap();
    assert_eq!(current(&f, &id), None);
}

#[test]
fn sh870_retained_a_held_resume_shows_only_while_its_story_still_owes_a_generation() {
    let f = fixture();
    let view = resume::decided(&f);
    let id = view.record.id.clone();
    resume::land(&f, &view);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let resumed = service.reconcile_landing(&id).unwrap();
    let effect = resumed
        .state
        .work
        .iter()
        .find(|w| w.kind == WorkKind::Resume)
        .unwrap()
        .id
        .clone();
    service.claim_work(&id, &effect).unwrap().unwrap();
    service
        .settle_work(
            &id,
            &effect,
            1,
            AssessmentDelivery::Uncertain("pane ownership unknown".into()),
        )
        .unwrap();
    assert_eq!(phase(&f, &id).as_deref(), Some("held"));
    let disposition = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .awaiting;
    assert!(
        disposition.is_some_and(|awaiting| awaiting.contains(&id)),
        "terminal delivery writes its own recovery hold"
    );
    let stories = StoryService::new(&ctx);
    stories.clear_awaiting("SH-1").unwrap();
    assert_eq!(
        phase(&f, &id).as_deref(),
        Some("held"),
        "SH-1 still owes its generation"
    );
    stories
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    assert_eq!(current(&f, &id), None, "the held resume is moot");
}

#[test]
fn sh870_retained_an_in_flight_resume_keeps_the_row_until_it_settles() {
    let f = fixture();
    let view = resume::decided(&f);
    let id = view.record.id.clone();
    resume::land(&f, &view);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let resumed = service.reconcile_landing(&id).unwrap();
    let effect = resumed
        .state
        .work
        .iter()
        .find(|w| w.kind == WorkKind::Resume)
        .unwrap()
        .id
        .clone();
    service.claim_work(&id, &effect).unwrap().unwrap();
    StoryService::new(&ctx)
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    assert!(
        current(&f, &id).is_some(),
        "an outstanding external call always shows"
    );
    service
        .settle_work(&id, &effect, 1, AssessmentDelivery::Delivered)
        .unwrap();
    assert_eq!(current(&f, &id), None);
}
