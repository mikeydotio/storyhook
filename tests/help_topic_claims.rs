//! Pins the `export`, `import` and `import-project` help topics against the
//! document and behaviour `story export` actually has (SH-215), and the
//! `list`, `delete` and `archive` topics against `story list`'s visibility
//! filter (SH-409).
//!
//! `tests/help_topic_references.rs` is hermetic by construction — it scans
//! source text and calls [`storyhook::help_topics::get_help_topic`] directly,
//! checking only that a topic a message points at *exists*. It has no way to
//! ask whether a topic's *claims* about a command are still true, which is
//! exactly how SH-215 happened: `story export`'s document grew from a bare
//! array of open stories to a multi-key object carrying every story, its
//! catalog, and (when configured) its settings and remotes — and the help
//! topic describing it never moved. This file drives the real CLI and the
//! real [`ProjectExport`] type to check the claims themselves, so the next
//! field export grows has to update the topic before this file goes green
//! again.

use std::collections::BTreeMap;

use storyhook::domain::{Member, StateDef, SuperState, TypeDef};
use storyhook::help_topics::get_help_topic;
use storyhook::service::transfer::{
    ExportedRemote, ExportedSettings, ExportedStory, ProjectExport,
};
use storyhook_test_support::{TestEnv, scratch_dir};

/// A `story` command, using the shared isolated test environment.
fn story(dir: &std::path::Path) -> assert_cmd::Command {
    TestEnv::shared().story(dir)
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
        // Named explicitly so the literal stays exhaustive (this test's own
        // point), but never populated: SH-408 retired the engine that
        // configured either, `export` never writes them from a current
        // store, and `skip_serializing_if` omits both from the JSON object
        // below — they carry no claim for the help topic to make (see
        // `ProjectExport::github_sync`'s own doc comment).
        github_sync: None,
        github_bases: BTreeMap::new(),
        stories: vec![ExportedStory {
            id: "TST-1".to_string(),
            events: Vec::new(),
            archived: false,
            attachment_blobs: Vec::new(),
        }],
    };

    let value = serde_json::to_value(&export).expect("serializing a fully-populated export");
    let object = value
        .as_object()
        .expect("an export document is a JSON object");

    // Vacuity guard: a fully-populated document that still omitted an
    // optional field would make the assertion below trivially satisfiable.
    assert!(
        object.len() >= 8,
        "a fully-populated ProjectExport should carry at least 8 top-level keys \
         (schema, prefix, states, types, members, settings, remotes, stories); \
         found {}: {:?}",
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

// ---------------------------------------------------------------------------
// SH-409: `story list`'s visibility default and the topics that describe it.
//
// SH-215 caught a document describing what `story export` *used to* return;
// SH-409 is the same class in the opposite direction — three topics
// (`list`, `delete`, `archive`) already claimed a behaviour `list` did not
// have (`src/help_topics.rs:308-309`, `:2217`, and `story help delete`'s own
// "still appears in `story list`" said the opposite of what it now must).
// Checking a topic's prose against real CLI output once is what stops a
// future change to the visibility filter from silently re-opening either
// gap.
// ---------------------------------------------------------------------------

/// A fresh project with one story of each visibility category. Returns the
/// project handle; ids are assigned in creation order: SH-1 open, SH-2
/// closed, SH-3 archived, SH-4 permanently removed.
fn project_with_one_of_each_visibility_category() -> storyhook_test_support::Project<'static> {
    let project = TestEnv::shared().project().build();
    project.run(&["new", "Open"]).success();
    project.run(&["new", "Closed"]).success();
    project.run(&["new", "Archived"]).success();
    project.run(&["new", "Deleted"]).success();
    project.run(&["move", "SH-2", "done"]).success();
    project.run(&["move", "SH-3", "done"]).success();
    project.run(&["archive", "SH-3"]).success();
    project.run(&["delete", "SH-4", "--force"]).success();
    project
}

fn list_ids(project: &storyhook_test_support::Project<'_>, args: &[&str]) -> Vec<String> {
    let mut full = vec!["list"];
    full.extend_from_slice(args);
    project.json(&full)["stories"]
        .as_array()
        .expect("a stories array")
        .iter()
        .map(|view| view["story"]["id"].as_str().unwrap().to_string())
        .collect()
}

/// The `list` topic's lede claims closed and archived stories
/// are excluded by default — this drives the real CLI rather than trusting
/// the prose, which is exactly the gap SH-409 found (the topic used to claim
/// this and `list` did not do it).
#[test]
fn the_list_topic_matches_lists_actual_default_visibility() {
    let topic = get_help_topic("list").expect("the list topic exists");
    assert!(
        topic.contains("closed") && topic.contains("archived"),
        "the list topic must name both categories its default excludes: {topic}"
    );

    let project = project_with_one_of_each_visibility_category();
    let ids = list_ids(&project, &[]);
    assert_eq!(
        ids,
        ["SH-1"],
        "closed, archived and deleted must be absent from the default list, \
         matching what the topic claims: {ids:?}"
    );
}

/// Every visibility flag the `list` topic documents must actually be
/// accepted by the parser, and do what the topic says it does.
#[test]
fn the_list_topic_names_working_visibility_flags() {
    let topic = get_help_topic("list").expect("the list topic exists");
    for flag in ["--include-closed", "--include-archived", "--all"] {
        assert!(
            topic.contains(flag),
            "the list topic must document {flag}: {topic}"
        );
    }

    let project = project_with_one_of_each_visibility_category();
    assert_eq!(list_ids(&project, &["--include-closed"]), ["SH-1", "SH-2"]);
    assert_eq!(
        list_ids(&project, &["--include-archived"]),
        ["SH-1", "SH-2", "SH-3"],
        "the topic says --include-archived implies --include-closed"
    );
    assert_eq!(list_ids(&project, &["--all"]), ["SH-1", "SH-2", "SH-3"]);
}

/// The `list` topic says naming a closed state lifts the closed exclusion
/// but not the archived one — checked against the real archived-and-done
/// story built above, not just the closed one.
#[test]
fn the_list_topic_is_right_about_what_state_lifts() {
    let topic = get_help_topic("list").expect("the list topic exists");
    assert!(
        topic.contains("lifts the") && topic.contains("closed exclusion"),
        "the list topic must describe the --state lift: {topic}"
    );

    let project = project_with_one_of_each_visibility_category();
    let ids = list_ids(&project, &["--state", "done"]);
    assert_eq!(
        ids,
        ["SH-2"],
        "--state done reveals the plain closed story but not the archived one"
    );
}

/// The `delete` topic's own claim, driven against the real CLI: the story no
/// longer exists, so neither visibility flags nor direct lookup recover it.
#[test]
fn the_delete_topic_is_right_that_removal_is_permanent() {
    let topic = get_help_topic("delete").expect("the delete topic exists");
    assert!(
        topic.contains("Permanently remove") && topic.contains("There is no undo"),
        "the delete topic must state the permanent contract: {topic}"
    );

    let project = project_with_one_of_each_visibility_category();
    let ids = list_ids(&project, &["--all"]);
    assert!(
        !ids.contains(&"SH-4".to_string()),
        "SH-4 (removed) must not appear even under --all: {ids:?}"
    );
    project
        .story()
        .args(["show", "SH-4"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not found"));
}

/// The `archive` topic's claim that `story list` excludes an archived story
/// by default and `--include-archived`/`--all` reveal it.
#[test]
fn the_archive_topic_is_right_about_lists_default() {
    let topic = get_help_topic("archive").expect("the archive topic exists");
    assert!(
        topic.contains("--include-archived") && topic.contains("--all"),
        "the archive topic must name the flags that reveal an archived story \
         in `story list`: {topic}"
    );

    let project = project_with_one_of_each_visibility_category();
    assert!(!list_ids(&project, &[]).contains(&"SH-3".to_string()));
    assert!(list_ids(&project, &["--include-archived"]).contains(&"SH-3".to_string()));
}

#[test]
fn the_engine_topic_documents_immutable_launch_configuration() {
    let topic = get_help_topic("engine").expect("the engine topic exists");
    for flag in ["--model", "--effort", "--speed"] {
        assert!(
            topic.contains(flag),
            "the engine topic must document {flag}: {topic}"
        );
    }
    assert!(
        topic.contains("every lane") && topic.contains("status"),
        "the engine topic must explain reuse and visibility: {topic}"
    );
}
