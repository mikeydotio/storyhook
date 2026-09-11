//! SH-680 absorbed into SH-678: creation and edits share the comment checker.

use storyhook::domain::ImportStory;
use storyhook::service::{FieldEdits, NewStoryInput, StoryService, TransferService};
use storyhook::store::{ReadOps, Store, StoryNo, StoryQuery};
use storyhook_test_support::ServiceFixture;

fn create(f: &ServiceFixture) -> String {
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Fix file parsing".into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

#[test]
fn creation_checks_both_fields_before_allocating_an_id() {
    for (title, description, field) in [
        ("Don't stop", "Use the file.", "title"),
        ("Fix file parsing", "Utilize the file.", "description"),
    ] {
        let f = ServiceFixture::new();
        let error = StoryService::new(&f.ctx())
            .create(&NewStoryInput {
                title: title.into(),
                description: Some(description.into()),
                ..Default::default()
            })
            .expect_err("invalid creation");
        assert!(error.to_string().contains(field));
        assert!(
            f.store()
                .read(|tx| tx.stories(f.project(), &StoryQuery::all()))
                .unwrap()
                .is_empty()
        );
        assert_eq!(create(&f), "SH-1");
    }
}

#[test]
fn direct_and_json_edits_reject_invalid_text_atomically() {
    for edits in [
        FieldEdits {
            title: Some("Don't stop".into()),
            priority: Some("high".into()),
            ..Default::default()
        },
        FieldEdits {
            description: Some("Utilize the file.".into()),
            priority: Some("high".into()),
            ..Default::default()
        },
        FieldEdits {
            json: Some(r#"{"title":"Don't stop","priority":"high"}"#.into()),
            ..Default::default()
        },
        FieldEdits {
            json: Some(r#"{"description":"Utilize it.","priority":"high"}"#.into()),
            ..Default::default()
        },
    ] {
        let f = ServiceFixture::new();
        let id = create(&f);
        let no = StoryNo::parse_id("SH", &id).unwrap();
        let before = f
            .store()
            .read(|tx| tx.story(f.project(), no))
            .unwrap()
            .unwrap();
        StoryService::new(&f.ctx())
            .set_fields(&id, &edits)
            .expect_err("reject whole edit");
        let after = f
            .store()
            .read(|tx| tx.story(f.project(), no))
            .unwrap()
            .unwrap();
        assert_eq!(after.head_seq, before.head_seq);
        assert_eq!(after.snapshot.title, before.snapshot.title);
        assert_eq!(after.snapshot.priority, before.snapshot.priority);
    }
}

#[test]
fn import_rejects_the_whole_batch_when_a_later_story_fails() {
    let f = ServiceFixture::new();
    let stories: Vec<ImportStory> = serde_json::from_str(
        r#"[{"title":"First story"},{"title":"Second story","description":"Don't stop."}]"#,
    )
    .unwrap();
    TransferService::new(&f.ctx())
        .import(&stories)
        .expect_err("reject whole batch");
    assert!(
        f.store()
            .read(|tx| tx.stories(f.project(), &StoryQuery::all()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(create(&f), "SH-1");
}

#[test]
fn title_fragments_and_empty_description_edits_remain_valid() {
    let f = ServiceFixture::new();
    let id = create(&f);
    StoryService::new(&f.ctx())
        .set_fields(
            &id,
            &FieldEdits {
                title: Some("Fix `utilize()` parsing".into()),
                description: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
}

#[test]
fn cli_decomposition_reports_a_later_invalid_story_without_partial_creation() {
    let env = storyhook_test_support::TestEnv::isolated();
    let project = env.project().prefix("SH").build();
    let output = env
        .story(project.path())
        .args(["decompose", "--stdin", "--json"])
        .write_stdin("stories:\n  - title: First story\n  - title: Don't stop\n")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["kind"], "text_lint");
    env.story(project.path())
        .args(["new", "Next story", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("SH-1"));
}

#[test]
fn title_and_description_advice_reaches_cli_json_without_rejecting_the_write() {
    let env = storyhook_test_support::TestEnv::isolated();
    let project = env.project().prefix("SH").build();
    for args in [
        vec!["new", "The file was removed", "--json"],
        vec![
            "set",
            "SH-1",
            "--description",
            "The file was removed.",
            "--json",
        ],
    ] {
        let output = env.story(project.path()).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            json["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning.as_str().unwrap().contains("possible-passive"))
        );
    }
}

#[test]
fn undo_restores_historical_title_and_description() {
    use storyhook::domain::StoryEvent;
    let f = ServiceFixture::new();
    let id = create(&f);
    let no = StoryNo::parse_id("SH", &id).unwrap();
    storyhook::store::test_support::inject_events(
        f.store(),
        f.project(),
        no,
        &[
            StoryEvent::StoryTitleSet {
                at: storyhook_test_support::FIXTURE_NOW.into(),
                title: "Don't change this historic title".into(),
            },
            StoryEvent::StoryDescriptionSet {
                at: storyhook_test_support::FIXTURE_NOW.into(),
                description: "Utilize the old API.".into(),
            },
        ],
    )
    .unwrap();
    let target = f
        .store()
        .read(|tx| tx.events_for(f.project(), no))
        .unwrap()
        .into_iter()
        .filter_map(|event| event.known().cloned())
        .collect::<Vec<_>>();
    StoryService::new(&f.ctx())
        .set_fields(
            &id,
            &FieldEdits {
                title: Some("Use the new API".into()),
                description: Some("Use the new API.".into()),
                ..Default::default()
            },
        )
        .unwrap();
    storyhook::service::history::restore(&f.ctx(), &id, &target).unwrap();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), no))
        .unwrap()
        .unwrap();
    assert_eq!(row.snapshot.title, "Don't change this historic title");
    assert_eq!(
        row.snapshot.description.as_deref(),
        Some("Utilize the old API.")
    );
}
