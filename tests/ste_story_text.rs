//! SH-727: story text remains verbatim without automated STE checks.

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
fn creation_and_direct_or_json_edits_preserve_both_fields() {
    use storyhook::domain::provenance::{ActorLabel, Provenance};
    let title = "Don't stop";
    let description = "Utilize this description with more than twenty words because all of the original text must survive creation and subsequent edits without any rewriting.";
    for label in [None, Some("web:user"), Some("codex"), Some("automation")] {
        let f = ServiceFixture::new();
        let actor = label.and_then(|label| ActorLabel::parse(label).unwrap());
        let ctx = f
            .ctx()
            .with_provenance(Provenance::command("new").with_actor(actor));
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: title.into(),
                description: Some(description.into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(story.title, title);
        assert_eq!(story.description.as_deref(), Some(description));
        for json in [false, true] {
            let edits = if json {
                FieldEdits {
                json: Some(serde_json::json!({"title": title, "description": description, "priority": "high"}).to_string()),
                ..Default::default()
            }
            } else {
                FieldEdits {
                    title: Some(title.into()),
                    description: Some(description.into()),
                    priority: Some("high".into()),
                    ..Default::default()
                }
            };
            let id = create(&f);
            StoryService::new(&ctx).set_fields(&id, &edits).unwrap();
            let row = f
                .store()
                .read(|tx| tx.story(f.project(), StoryNo::parse_id("SH", &id).unwrap()))
                .unwrap()
                .unwrap();
            assert_eq!(row.snapshot.title, title);
            assert_eq!(row.snapshot.description.as_deref(), Some(description));
            assert_eq!(row.snapshot.priority, storyhook::domain::Priority::High);
        }
    }
}

#[test]
fn invalid_non_text_fields_still_roll_back_compound_edits() {
    let f = ServiceFixture::new();
    let id = create(&f);
    let no = StoryNo::parse_id("SH", &id).unwrap();
    let before = f
        .store()
        .read(|tx| tx.story(f.project(), no))
        .unwrap()
        .unwrap();
    for json in [false, true] {
        let edits = if json {
            FieldEdits {
                json: Some(r#"{"title":"Don't stop","priority":"invalid"}"#.into()),
                ..Default::default()
            }
        } else {
            FieldEdits {
                title: Some("Don't stop".into()),
                priority: Some("invalid".into()),
                ..Default::default()
            }
        };
        let error = StoryService::new(&f.ctx())
            .set_fields(&id, &edits)
            .unwrap_err();
        assert!(error.to_string().contains("priority"));
        let after = f
            .store()
            .read(|tx| tx.story(f.project(), no))
            .unwrap()
            .unwrap();
        assert_eq!(after.head_seq, before.head_seq);
        assert_eq!(after.snapshot.title, before.snapshot.title);
    }
}

#[test]
fn imports_preserve_non_ste_text_across_the_batch() {
    let f = ServiceFixture::new();
    let stories: Vec<ImportStory> = serde_json::from_str(r#"[{"title":"Don't stop"},{"title":"Second story","description":"Utilize it. The file was removed."}]"#).unwrap();
    TransferService::new(&f.ctx()).import(&stories).unwrap();
    let rows = f
        .store()
        .read(|tx| tx.stories(f.project(), &StoryQuery::all()))
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.snapshot.title == "Don't stop"));
    assert!(rows.iter().any(
        |row| row.snapshot.description.as_deref() == Some("Utilize it. The file was removed.")
    ));
}

#[test]
fn cli_creation_edits_import_and_decomposition_have_no_ste_advice() {
    let env = storyhook_test_support::TestEnv::isolated();
    let project = env.project().prefix("SH").build();
    for (args, input) in [
        (vec!["new", "The file was removed", "--json"], ""),
        (
            vec![
                "set",
                "SH-1",
                "--description",
                "The file was removed.",
                "--json",
            ],
            "",
        ),
        (
            vec![
                "set",
                "SH-1",
                "--json",
                r#"{"title":"The file was removed"}"#,
            ],
            "",
        ),
        (
            vec!["import", "--json"],
            r#"[{"title":"Don't stop","description":"The file was removed."}]"#,
        ),
        (
            vec!["decompose", "--stdin", "--json"],
            "stories:\n  - title: First story\n  - title: Don't stop\n    description: The file was removed.\n",
        ),
    ] {
        let output = env
            .story(project.path())
            .args(&args)
            .write_stdin(input)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("possible-passive"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("possible-passive"));
    }
    env.story(project.path())
        .args(["show", "SH-4", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Don't stop"));
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

#[test]
fn compound_text_edit_retains_unrelated_blocker_warning() {
    let env = storyhook_test_support::TestEnv::isolated();
    let project = env
        .project()
        .prefix("SH")
        .seed_story("First story")
        .seed_story("Blocker")
        .build();
    let output = env
        .story(project.path())
        .args([
            "set",
            "SH-1",
            "--title",
            "The file was removed",
            "--blocked",
            "Waiting on SH-2",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warnings = json["warnings"]
        .as_array()
        .expect("the missing blocker edge produces a warning");
    assert!(warnings.iter().any(
        |w| w.as_str().unwrap().contains("SH-2") && w.as_str().unwrap().contains("blocked-by")
    ));
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("possible-passive"))
    );
}
