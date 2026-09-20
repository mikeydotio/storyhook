use super::*;
use storyhook::daemon::verification::{VerificationActivity, status::VerifierStatus};
use storyhook::service::project_recovery::RepairScope;

#[test]
fn shared_status_exposes_repair_ownership_and_legacy_payloads_default_empty() {
    let f = fixture();
    let view = decision::ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let accepted = service
        .decide(
            &view.record.id,
            &decision::input(&view, RepairScope::SeparateStory),
        )
        .unwrap();
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
