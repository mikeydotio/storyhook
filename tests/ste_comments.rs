//! SH-727: writing guidance must not reject or annotate authored comments.

use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo};
use storyhook_test_support::ServiceFixture;

fn seed(f: &ServiceFixture) -> String {
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Test comment guidance".into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

#[test]
fn all_callers_store_non_ste_comments_verbatim() {
    use storyhook::domain::provenance::{ActorLabel, Provenance};
    for label in [
        None,
        Some("web:user"),
        Some("tui:user"),
        Some("cli:user"),
        Some("codex"),
        Some("automation"),
    ] {
        let f = ServiceFixture::new();
        let id = seed(&f);
        let actor = label.and_then(|label| ActorLabel::parse(label).unwrap());
        let ctx = f
            .ctx()
            .with_provenance(Provenance::command("comment").with_actor(actor));
        for text in [
            "Don't utilize it.",
            "Commence work.",
            "The file was removed.",
            "This sentence deliberately contains more than twenty words so the old sentence length check would reject the text before it could be stored.",
        ] {
            StoryService::new(&ctx).comment(&id, text).unwrap();
        }
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), StoryNo::parse_id("SH", &id).unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(row.snapshot.comments.len(), 4);
        assert_eq!(row.snapshot.comments[0].text, "Don't utilize it.");
        assert_eq!(row.snapshot.comments[1].text, "Commence work.");
        assert_eq!(row.snapshot.comments[2].text, "The file was removed.");
        assert!(row.snapshot.comments[3].text.ends_with("stored."));
    }
}

#[test]
fn transition_stores_non_ste_comment_with_state_change() {
    let f = ServiceFixture::new();
    let id = seed(&f);
    StoryService::new(&f.ctx())
        .set_state(&id, "in-progress", Some("Don't start."), None, None)
        .unwrap();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(row.snapshot.state, "in-progress");
    assert_eq!(row.snapshot.comments[0].text, "Don't start.");
}

#[test]
fn cli_stores_approved_plan_verbatim_without_lint_advice() {
    let env = storyhook_test_support::TestEnv::isolated();
    let project = env.project().seed_story("Test text guidance").build();
    let plan = "# Approved plan — café\n\nDon't utilize a rewritten plan because this sentence deliberately contains more than twenty words and must remain exactly as the user approved it.\n\nThe file was removed.\n\n- Keep `$(touch never)` and $HOME literal.\n\n```sh\nprintf '%s' \"quoted\"\n```\n> evidence";
    for text in [plan, "The file was removed."] {
        let output = env
            .story(project.path())
            .args(["comment", "SH-1", text, "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!json.to_string().contains("possible-passive"));
        assert!(!json.to_string().contains("text_lint"));
    }
    let output = env
        .story(project.path())
        .args(["show", "SH-1", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["story"]["story"]["comments"][0]["text"], plan);
    assert_eq!(
        json["story"]["story"]["comments"][1]["text"],
        "The file was removed."
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

#[test]
fn service_preserves_surrounding_whitespace_and_line_endings() {
    let f = ServiceFixture::new();
    let id = seed(&f);
    let text = "  Don't utilize it.\r\n\r\n```text\r\n$HOME remains literal\r\n```\r\n ";
    let result = StoryService::new(&f.ctx()).comment(&id, text).unwrap();
    assert_eq!(result.comments[0].text, text);
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(row.snapshot.comments[0].text, text);
}
