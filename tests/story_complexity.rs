//! Complexity is durable metadata; omitted values are not an assessment.
use storyhook::domain::{Complexity, StoryEvent};
use storyhook::service::{FieldEdits, NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo};
use storyhook_test_support::ServiceFixture;

#[test]
fn omitted_complexity_is_medium_and_unassessed() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Unassessed".into(),
            ..Default::default()
        })
        .unwrap();
    let value = serde_json::to_value(story).unwrap();
    assert_eq!(value["complexity"], "medium");
    assert_eq!(value["complexity_assessed"], false);
}

#[test]
fn json_edits_assess_each_level_and_reject_invalid_values_atomically() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = service
        .create(&NewStoryInput {
            title: "Original".into(),
            ..Default::default()
        })
        .unwrap();
    for level in ["low", "medium", "high"] {
        service
            .set_fields(
                &story.id,
                &FieldEdits {
                    json: Some(serde_json::json!({"complexity":level}).to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        let value = fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .snapshot;
        assert_eq!(value.complexity.as_str(), level);
        assert!(value.complexity_assessed);
    }
    for invalid in [
        serde_json::json!("urgent"),
        serde_json::json!(null),
        serde_json::json!(5),
    ] {
        assert!(
            service
                .set_fields(
                    &story.id,
                    &FieldEdits {
                        title: Some("Must not persist".into()),
                        json: Some(serde_json::json!({"complexity":invalid}).to_string()),
                        ..Default::default()
                    }
                )
                .is_err()
        );
    }
    let value = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .snapshot;
    assert_eq!(value.title, "Original");
    assert_eq!(value.complexity, Complexity::High);
}

#[test]
fn explicit_creation_and_import_preserve_assessment() {
    use storyhook::service::transfer::{TransferService, parse_import_documents};
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    for level in Complexity::ALL {
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: level.as_str().into(),
                complexity: Some(level.as_str().into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(story.complexity, level);
        assert!(story.complexity_assessed);
    }
    assert!(
        StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "invalid".into(),
                complexity: Some("urgent".into()),
                ..Default::default()
            })
            .is_err()
    );
    let imports =
        parse_import_documents(r#"[{"title":"old"},{"title":"new","complexity":"high"}]"#).unwrap();
    TransferService::new(&ctx).import(&imports).unwrap();
    let rows = fixture
        .store()
        .read(|tx| tx.stories(fixture.project(), &storyhook::store::StoryQuery::all()))
        .unwrap();
    let old = rows.iter().find(|s| s.title == "old").unwrap();
    let new = rows.iter().find(|s| s.title == "new").unwrap();
    assert_eq!(old.snapshot.complexity, Complexity::Medium);
    assert!(!old.snapshot.complexity_assessed);
    assert_eq!(new.snapshot.complexity, Complexity::High);
    assert!(new.snapshot.complexity_assessed);
    let exported = TransferService::new(&ctx).export().unwrap();
    let json = serde_json::to_value(exported).unwrap();
    assert!(json.to_string().contains("StoryComplexitySet"));
}

#[test]
fn clearing_an_assessment_replays_and_serializes_without_rewriting_history() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let created = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Undo".into(),
            complexity: Some("high".into()),
            ..Default::default()
        })
        .unwrap();
    storyhook::store::test_support::inject_events(
        fixture.store(),
        fixture.project(),
        StoryNo::parse_id("SH", &created.id).unwrap(),
        &[StoryEvent::StoryComplexityCleared {
            at: storyhook_test_support::FIXTURE_NOW.into(),
        }],
    )
    .unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.snapshot.complexity, Complexity::Medium);
    assert!(!row.snapshot.complexity_assessed);
}

#[test]
fn decomposition_accepts_complexity_in_yaml_and_markdown() {
    let yaml = storyhook::decompose::decompose(
        Some("stories.yaml"),
        "stories:\n  - title: Plan\n    complexity: high\n",
    )
    .unwrap();
    let markdown =
        storyhook::decompose::decompose(Some("stories.md"), "## Plan [complexity: high]\n")
            .unwrap();
    assert_eq!(yaml[0].complexity.as_deref(), Some("high"));
    assert_eq!(markdown[0].complexity.as_deref(), Some("high"));
    assert_eq!(markdown[0].title, "Plan");
}
