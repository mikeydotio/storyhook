//! Pins the `export`, `import` and `import-project` help topics against the
//! document and behaviour `story export` actually has (SH-215).
//!
//! `tests/help_topic_references.rs` is hermetic by construction — it scans
//! source text and calls [`storyhook::help_topics::get_help_topic`] directly,
//! checking only that a topic a message points at *exists*. It has no way to
//! ask whether a topic's *claims* about a command are still true, which is
//! exactly how SH-215 happened: `story export`'s document grew from a bare
//! array of open stories to a ten-key object carrying every story, its
//! catalog, and (when configured) its settings, remotes and github-sync
//! state — and the help topic describing it never moved. This file drives the
//! real CLI and the real [`ProjectExport`] type to check the claims
//! themselves, so the next field export grows has to update the topic before
//! this file goes green again.

use std::collections::BTreeMap;

use storyhook::domain::{Member, StateDef, StorySnapshot, SuperState, TypeDef};
use storyhook::help_topics::get_help_topic;
use storyhook::service::transfer::{
    ExportedRemote, ExportedSettings, ExportedStory, ProjectExport,
};
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo};
use storyhook_test_support::{ServiceFixture, TestEnv, scratch_dir};

/// A `story` command, using the shared isolated test environment.
fn story(dir: &std::path::Path) -> assert_cmd::Command {
    TestEnv::shared().story(dir)
}

/// A real [`StorySnapshot`], taken from a fixture rather than hand-built:
/// its fields and the invariants `fold_story` maintains between them make a
/// hand-constructed one a poor stand-in for what an export document actually
/// carries.
fn a_real_story_snapshot() -> StorySnapshot {
    let fixture = ServiceFixture::new();
    let id = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "A story with a github-sync base".to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id;
    let no = StoryNo::parse_id("SH", &id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story back")
        .expect("the story exists")
        .snapshot
}

/// Every top-level key a fully-populated [`ProjectExport`] carries must be
/// named in `story help export`.
///
/// The struct literal below names every field explicitly — `ProjectExport`
/// derives no `Default`, so a field the fix did not know about would fail to
/// *compile* this literal before it could ever fail to run this test. That is
/// the point: the defect this story fixed was a document that grew fields the
/// help text never learned about, and a hand-written list of expected keys
/// can silently stop covering a new one the same way the prose did. A
/// compiler-exhaustive literal cannot.
#[test]
fn the_export_topic_names_every_key_the_export_document_carries() {
    let mut github_bases = BTreeMap::new();
    github_bases.insert("SH-1".to_string(), a_real_story_snapshot());

    let export = ProjectExport {
        schema: 1,
        prefix: Some("TST".to_string()),
        states: vec![StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        }],
        types: vec![TypeDef {
            slug: "bug".to_string(),
            description: None,
            emoji: None,
        }],
        members: vec![Member {
            id: "ada".to_string(),
            display_name: "Ada Lovelace".to_string(),
            email: None,
            github: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }],
        settings: ExportedSettings::new(Some(true), Some("30d".to_string())),
        remotes: vec![ExportedRemote {
            normalized: "github.com/acme/widgets".to_string(),
            raw: "https://github.com/acme/widgets.git".to_string(),
            registered_at: "2026-01-01T00:00:00Z".to_string(),
        }],
        github_sync: Some(serde_json::json!({"github": {"owner": "acme", "repo": "widgets"}})),
        github_bases,
        stories: vec![ExportedStory {
            id: "TST-1".to_string(),
            events: Vec::new(),
            archived: false,
        }],
    };

    let value = serde_json::to_value(&export).expect("serializing a fully-populated export");
    let object = value
        .as_object()
        .expect("an export document is a JSON object");

    // Vacuity guard: a fully-populated document that still omitted an
    // optional field would make the assertion below trivially satisfiable.
    assert!(
        object.len() >= 10,
        "a fully-populated ProjectExport should carry at least 10 top-level keys \
         (schema, prefix, states, types, members, settings, remotes, github_sync, \
         github_bases, stories); found {}: {:?}",
        object.len(),
        object.keys().collect::<Vec<_>>()
    );

    let topic = get_help_topic("export").expect("the export topic exists");
    let missing: Vec<&String> = object
        .keys()
        .filter(|key| !topic.contains(key.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "the export document carries {missing:?} and `story help export` names none \
         of them -- export gained a field and its help topic did not, which is the \
         mechanism that produced SH-215. Document keys: {:?}",
        object.keys().collect::<Vec<_>>()
    );
}

/// The verb the export topic's first `Related:` line names.
fn export_document_restore_verb() -> String {
    let topic = get_help_topic("export").expect("the export topic exists");
    let (_, after) = topic
        .split_once("Related:\n")
        .expect("the export topic has a Related: section");
    let first_line = after
        .lines()
        .next()
        .expect("the Related: section names at least one command");
    let verb = first_line
        .trim_start()
        .strip_prefix("story ")
        .expect("the Related: section's first line names a `story <verb>`");
    verb.split_whitespace()
        .next()
        .expect("a verb token")
        .to_string()
}

/// The command the export topic's `Related:` section names first as this
/// document's restore verb must actually restore it. SH-215's own mistake was
/// exactly this: the topic named `story import`, which refuses this shape at
/// exit 5.
#[test]
fn the_restore_verb_the_export_topic_names_actually_restores_an_export_document() {
    let verb = export_document_restore_verb();

    let env = TestEnv::shared();
    let source = env.project().build();
    story(source.path())
        .args(["new", "Something worth backing up"])
        .assert()
        .success();
    let backup = story(source.path())
        .args(["export"])
        .output()
        .expect("running export");
    assert!(
        backup.status.success(),
        "`story export` failed: {}",
        String::from_utf8_lossy(&backup.stderr)
    );
    let document = String::from_utf8(backup.stdout).expect("the export document must be UTF-8");

    let restore_dir = scratch_dir();
    let file = restore_dir.path().join("backup.json");
    std::fs::write(&file, &document).expect("writing the export document");

    story(restore_dir.path())
        .args([verb.as_str(), file.to_str().unwrap()])
        .assert()
        .success();
}

/// The reported falsehood, named directly so a regression names itself
/// (`the_storage_topic_exists`'s precedent in `tests/help_topic_references.rs`).
#[test]
fn the_export_topic_does_not_call_the_document_an_array_of_stories() {
    let topic = get_help_topic("export").expect("the export topic exists");
    assert!(
        !topic.to_lowercase().contains("as a json array"),
        "the export topic must not call the document a JSON array -- it is one \
         object with ten top-level keys: {topic}"
    );
}

/// The document carries closed stories too, not just open ones -- both the
/// prose and the behaviour it describes.
#[test]
fn the_export_topic_says_the_document_carries_closed_stories_too() {
    let topic = get_help_topic("export").expect("the export topic exists");
    assert!(
        topic.contains("closed"),
        "the export topic must say the document carries closed stories too, not \
         only open ones: {topic}"
    );

    let env = TestEnv::shared();
    let project = env.project().build();
    story(project.path())
        .args(["new", "Stays open"])
        .assert()
        .success();
    story(project.path())
        .args(["new", "Gets closed"])
        .assert()
        .success();
    story(project.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    let document: serde_json::Value = project.json(&["export"]);
    let ids: Vec<String> = document["stories"]
        .as_array()
        .expect("export carries a stories array")
        .iter()
        .map(|entry| entry["id"].as_str().expect("a story id").to_string())
        .collect();
    assert!(
        ids.contains(&"SH-1".to_string()) && ids.contains(&"SH-2".to_string()),
        "export must carry both the open and the closed story: {ids:?}"
    );
}

/// The reported falsehood in `json-format`: `story export` never lands in
/// the `"message"` field, and neither does its behaviour under `--json`.
#[test]
fn the_json_format_topic_does_not_claim_export_is_enveloped() {
    let topic = get_help_topic("json-format").expect("the json-format topic exists");
    assert!(
        !topic.contains("<json array of stories>"),
        "json-format must not claim `story export`'s message is \"<json array of \
         stories>\" -- export bypasses the envelope entirely: {topic}"
    );

    let env = TestEnv::shared();
    let project = env.project().build();
    story(project.path())
        .args(["new", "Something"])
        .assert()
        .success();
    let document: serde_json::Value = project.json(&["export"]);
    assert!(
        document.get("schema").is_some(),
        "`story export --json` must be the document itself: {document}"
    );
    assert!(
        document.get("result").is_none(),
        "`story export --json` must not be wrapped in the envelope: {document}"
    );
}

/// `story import-project` is a dispatchable verb (`src/cli.rs`'s `dispatch`
/// forwards it to `dispatch_unscoped_with_stdin`) and, before this story, had
/// no help topic at all -- `--help` fell back to the generic usage dump, and
/// `--legacy-links` was reachable only from a parse-error string.
#[test]
fn the_import_project_topic_exists_and_documents_legacy_links() {
    let topic = get_help_topic("import-project")
        .expect("`story import-project` must have its own help topic (SH-215)");
    assert!(
        topic.contains("--legacy-links"),
        "the import-project topic must document --legacy-links: {topic}"
    );
    assert!(
        topic.to_lowercase().contains("empty"),
        "the import-project topic must state the empty-project precondition: {topic}"
    );
}

/// The import topic must point a reader holding a `story export` document at
/// the verb that can actually read it.
#[test]
fn the_import_topic_points_an_export_document_at_the_verb_that_can_read_it() {
    let topic = get_help_topic("import").expect("the import topic exists");
    assert!(
        topic.contains("import-project"),
        "the import topic must name `story import-project` as where a `story \
         export` document is actually read: {topic}"
    );
}
