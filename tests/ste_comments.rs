//! New comment text must fail before any part of its transaction is stored.

use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo};
use storyhook_test_support::ServiceFixture;

fn seed(f: &ServiceFixture) -> String {
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Test comment checks".into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

#[test]
fn rejected_comment_keeps_history_and_snapshot_unchanged() {
    let f = ServiceFixture::new();
    let id = seed(&f);
    let no = StoryNo::parse_id("SH", &id).unwrap();
    let before = f.store().read(|tx| tx.events_for(f.project(), no)).unwrap();
    let error = StoryService::new(&f.ctx())
        .comment(&id, "Don't utilize it.")
        .expect_err("bad prose must be rejected");
    assert!(error.to_string().contains("comment"));
    assert!(error.to_string().contains("contraction"));
    assert_eq!(error.exit_code(), 2);
    let after = f.store().read(|tx| tx.events_for(f.project(), no)).unwrap();
    assert_eq!(before.len(), after.len());
    assert!(
        f.store()
            .read(|tx| tx.story(f.project(), no))
            .unwrap()
            .unwrap()
            .snapshot
            .comments
            .is_empty()
    );
    StoryService::new(&f.ctx())
        .comment(&id, "Do not use it.")
        .unwrap();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), no))
        .unwrap()
        .unwrap();
    assert_eq!(row.snapshot.comments.len(), 1);
    assert_eq!(row.snapshot.comments[0].text, "Do not use it.");
}

#[test]
fn bad_transition_comment_rolls_back_the_state_change() {
    let f = ServiceFixture::new();
    let id = seed(&f);
    StoryService::new(&f.ctx())
        .set_state(&id, "in-progress", Some("Don't start."), None, None)
        .expect_err("reject whole operation");
    let no = StoryNo::parse_id("SH", &id).unwrap();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), no))
        .unwrap()
        .unwrap();
    assert_eq!(row.snapshot.state, "todo");
    assert!(row.snapshot.comments.is_empty());
}

#[test]
fn unknown_and_human_callers_obey_the_same_policy() {
    use storyhook::domain::provenance::{ActorLabel, Provenance};
    let f = ServiceFixture::new();
    let id = seed(&f);
    for actor in [
        None,
        ActorLabel::parse("dashboard").unwrap(),
        ActorLabel::parse("codex").unwrap(),
    ] {
        let ctx = f
            .ctx()
            .with_provenance(Provenance::command("comment").with_actor(actor));
        StoryService::new(&ctx)
            .comment(&id, "Commence work.")
            .expect_err("all callers");
    }
}

#[test]
fn technical_evidence_is_stored_verbatim() {
    let f = ServiceFixture::new();
    let id = seed(&f);
    let text = "The command failed.\n\n```text\nDon't utilize this old API.\n```\n\nUse `new_api()` instead.";
    let result = StoryService::new(&f.ctx()).comment(&id, text).unwrap();
    assert_eq!(result.comments[0].text, text);
}

#[test]
fn rejected_text_keeps_structured_findings_across_wire_and_json() {
    let f = ServiceFixture::new();
    let id = seed(&f);
    let error = StoryService::new(&f.ctx())
        .comment(&id, "Don't utilize it.")
        .unwrap_err()
        .with_context("writing a comment");
    let wire = storyhook::error::WireError::from(&error);
    let decoded: storyhook::error::WireError =
        serde_json::from_str(&serde_json::to_string(&wire).unwrap()).unwrap();
    let received = storyhook::error::AppError::from(decoded);
    assert_eq!(received.to_string(), error.to_string());
    assert_eq!(storyhook::api::http::status_for(&received), 422);
    let json: serde_json::Value =
        serde_json::from_str(&storyhook::output::render_error(&received, true)).unwrap();
    assert_eq!(json["kind"], "text_lint");
    assert_eq!(json["findings"][0]["field"], "comment");
    assert_eq!(json["findings"][0]["rule"], "contraction");
    assert_eq!(json["findings"][1]["rule"], "vocabulary");
    assert_eq!(json["findings"][0]["span"]["start"], 0);
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("writing a comment")
    );
}

#[test]
fn cli_reports_repair_actions_and_advice_without_losing_input() {
    let env = storyhook_test_support::TestEnv::isolated();
    let project = env.project().seed_story("Test text checks").build();
    let output = env
        .story(project.path())
        .args(["comment", "SH-1", "Don't utilize it.", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["kind"], "text_lint");
    assert_eq!(json["findings"].as_array().unwrap().len(), 2);
    let repaired = env
        .story(project.path())
        .args(["comment", "SH-1", "The file was removed.", "--json"])
        .output()
        .unwrap();
    assert!(repaired.status.success());
    let json: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(
        json["story"]["story"]["comments"][0]["text"],
        "The file was removed."
    );
    assert!(
        json["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("possible-passive"))
    );
}

#[test]
fn undo_can_restore_historical_non_ste_text() {
    use storyhook::domain::StoryEvent;
    let f = ServiceFixture::new();
    let id = seed(&f);
    let no = StoryNo::parse_id("SH", &id).unwrap();
    let text = "Don't utilize this historic API.";
    storyhook::store::test_support::inject_events(
        f.store(),
        f.project(),
        no,
        &[StoryEvent::StoryCommentAdded {
            at: storyhook_test_support::FIXTURE_NOW.into(),
            text: text.into(),
        }],
    )
    .unwrap();
    let target = f
        .store()
        .read(|tx| tx.events_for(f.project(), no))
        .unwrap()
        .into_iter()
        .filter_map(|e| e.known().cloned())
        .collect::<Vec<_>>();
    storyhook::store::test_support::inject_events(
        f.store(),
        f.project(),
        no,
        &[StoryEvent::StoryCommentRetracted {
            at: storyhook_test_support::FIXTURE_NOW.into(),
            comment_at: storyhook_test_support::FIXTURE_NOW.into(),
            text: text.into(),
        }],
    )
    .unwrap();
    storyhook::service::history::restore(&f.ctx(), &id, &target).unwrap();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), no))
        .unwrap()
        .unwrap();
    assert_eq!(row.snapshot.comments[0].text, text);
}
