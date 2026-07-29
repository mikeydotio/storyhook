//! `SessionService` — the JSON block an agent's session hook injects, and the
//! seam's history read.
//!
//! The envelope shape is a contract with Claude Code, not an internal detail:
//! the golden corpus pins its bytes, and a session hook that prints anything
//! else — an error, a diagnostic, a bare string — lands in a model's context.

use storyhook::cli::{HistoryAction, Invocation};
use storyhook::domain::StoryEvent;
use storyhook::error::AppError;
use storyhook::invoke::dispatch;
use storyhook::output::Response;
use storyhook::service::{NewStoryInput, SessionService, StoryService};
use storyhook_test_support::ServiceFixture;

fn create(fixture: &ServiceFixture, title: &str, priority: Option<&str>) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.to_string(),
            priority: priority.map(str::to_string),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

/// The `additionalContext` string out of a session-start answer.
fn context(fixture: &ServiceFixture) -> String {
    let raw = SessionService::new(&fixture.ctx())
        .context()
        .expect("building the session context");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("an additionalContext string")
        .to_string()
}

#[test]
fn the_envelope_names_the_hook_event_and_injects_silently() {
    let fixture = ServiceFixture::new();
    let raw = SessionService::new(&fixture.ctx())
        .context()
        .expect("building");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(
        value["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "{raw}"
    );
    assert!(
        value["hookSpecificOutput"]["systemMessage"].is_null(),
        "a visible system message would be shown to the user: {raw}"
    );
}

#[test]
fn an_empty_project_still_carries_the_cli_reference() {
    let fixture = ServiceFixture::new();
    let context = context(&fixture);
    assert!(context.contains("PROJECT STATE"), "{context}");
    assert!(context.contains("0 open stories, 0 ready"), "{context}");
    assert!(
        !context.contains("Next:"),
        "there is no next story to name: {context}"
    );
}

#[test]
fn the_counts_separate_open_from_ready() {
    let fixture = ServiceFixture::new();
    let blocker = create(&fixture, "Blocker", None);
    let blocked = create(&fixture, "Blocked", None);
    storyhook::service::RelationService::new(&fixture.ctx())
        .relate(&blocker, "blocks", &blocked, false)
        .expect("relating");

    let context = context(&fixture);
    assert!(context.contains("2 open stories, 1 ready"), "{context}");
    assert!(
        context.contains(&format!("Next: {blocker} — Blocker")),
        "{context}"
    );
}

#[test]
fn the_next_line_reports_a_priority_when_there_is_one() {
    let fixture = ServiceFixture::new();
    create(&fixture, "Ordinary", None);
    let urgent = create(&fixture, "Urgent", Some("critical"));

    let context = context(&fixture);
    assert!(
        context.contains(&format!("Next: {urgent} — Urgent (critical)")),
        "{context}"
    );
}

#[test]
fn a_parent_with_children_is_never_the_next_story() {
    let fixture = ServiceFixture::new();
    let parent = create(&fixture, "Umbrella", Some("critical"));
    let child = create(&fixture, "Real work", None);
    storyhook::service::RelationService::new(&fixture.ctx())
        .relate(&parent, "parent-of", &child, false)
        .expect("relating");

    let context = context(&fixture);
    assert!(
        context.contains(&format!("Next: {child} — Real work")),
        "the parent must not be offered as work: {context}"
    );
}

#[test]
fn a_disabled_plugin_answers_with_nothing_at_all() {
    let fixture = ServiceFixture::new();
    create(&fixture, "Invisible", None);
    let dir = fixture.cwd().join(".storyhook");
    std::fs::create_dir_all(&dir).expect("creating the config directory");
    std::fs::write(dir.join("plugin-config.toml"), "enabled = false\n").expect("writing");

    assert_eq!(
        SessionService::new(&fixture.ctx())
            .context()
            .expect("building"),
        "{}"
    );
}

#[test]
fn a_malformed_plugin_config_leaves_the_context_switched_on() {
    let fixture = ServiceFixture::new();
    let dir = fixture.cwd().join(".storyhook");
    std::fs::create_dir_all(&dir).expect("creating the config directory");
    std::fs::write(dir.join("plugin-config.toml"), "this is not toml [").expect("writing");

    assert!(context(&fixture).contains("PROJECT STATE"));
}

#[test]
fn a_very_large_project_is_truncated_at_a_character_boundary() {
    let fixture = ServiceFixture::new();
    for index in 0..60 {
        create(
            &fixture,
            &format!("Story {index} — an em dash and a 🎯 to make the bytes wide"),
            None,
        );
    }
    let raw = SessionService::new(&fixture.ctx())
        .context()
        .expect("building");
    // Valid JSON is the assertion: a truncation mid-character would produce a
    // string serde could not encode.
    let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert!(value["hookSpecificOutput"]["additionalContext"].is_string());
}

#[test]
fn the_session_start_arm_answers_with_raw_json() {
    let fixture = ServiceFixture::new();
    let response = dispatch(&fixture.ctx(), Invocation::SessionStart).expect("dispatching");
    let Response::RawJson(json) = response else {
        panic!("`story session-start` must answer with RawJson so no envelope wraps it");
    };
    assert!(json.starts_with('{'), "{json}");
}

// --- history ---------------------------------------------------------------

#[test]
fn reading_a_storys_history_returns_its_events_in_order() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Has a past", None);
    StoryService::new(&fixture.ctx())
        .comment(&id, "First remark.")
        .expect("commenting");

    let response = dispatch(
        &fixture.ctx(),
        Invocation::History {
            action: HistoryAction::Read { id },
        },
    )
    .expect("reading history");
    let Response::StoryHistory(events) = response else {
        panic!("history read must answer with the events");
    };
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], StoryEvent::StoryCreated { .. }));
    assert!(matches!(events[1], StoryEvent::StoryCommentAdded { .. }));
}

#[test]
fn reading_the_history_of_a_story_that_does_not_exist_is_empty_not_an_error() {
    // The TUI reads a story's history to build an undo stack and must not fail
    // because the story went away underneath it.
    let fixture = ServiceFixture::new();
    let response = dispatch(
        &fixture.ctx(),
        Invocation::History {
            action: HistoryAction::Read {
                id: "SH-404".to_string(),
            },
        },
    )
    .expect("reading history");
    assert!(matches!(response, Response::StoryHistory(ref events) if events.is_empty()));
}

#[test]
fn restoring_a_history_refuses_loudly_and_names_where_the_design_is_owed() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Cannot be rewound", None);
    let error = dispatch(
        &fixture.ctx(),
        Invocation::History {
            action: HistoryAction::Restore {
                id,
                events: Vec::new(),
            },
        },
    )
    .expect_err("an append-only store cannot replace a history");
    assert!(matches!(error, AppError::Storage(_)), "{error}");
    assert!(error.to_string().contains("not yet ported"), "{error}");
    assert!(error.to_string().contains("flip-checklist"), "{error}");
}

#[test]
fn a_disabled_plugin_can_say_so_in_the_pointer_file() {
    // Where this config lives is what the flip changes: the `[plugin]` table in
    // the committed pointer file, rather than a file inside a directory that
    // stops existing. The *decision* is still the repository's, and still
    // user-authored — storyhook writes the pointer once and reads it after.
    let fixture = ServiceFixture::new();
    create(&fixture, "Invisible", None);
    std::fs::write(
        fixture.cwd().join(".storyhook.toml"),
        "schema = 1\nuuid = \"11111111-1111-4111-8111-111111111111\"\nprefix = \"SH\"\n\
         \n[plugin]\nenabled = false\n",
    )
    .expect("writing the pointer");

    assert_eq!(
        SessionService::new(&fixture.ctx())
            .context()
            .expect("building"),
        "{}"
    );
}

#[test]
fn the_pointers_plugin_table_wins_over_the_legacy_file() {
    let fixture = ServiceFixture::new();
    create(&fixture, "Visible", None);
    let dir = fixture.cwd().join(".storyhook");
    std::fs::create_dir_all(&dir).expect("creating the config directory");
    std::fs::write(dir.join("plugin-config.toml"), "enabled = false\n").expect("writing");
    std::fs::write(
        fixture.cwd().join(".storyhook.toml"),
        "schema = 1\nuuid = \"11111111-1111-4111-8111-111111111111\"\nprefix = \"SH\"\n\
         \n[plugin]\nenabled = true\n",
    )
    .expect("writing the pointer");

    assert!(
        context(&fixture).contains("PROJECT STATE"),
        "a repository that has moved its config into the pointer must not be \
         overruled by the file it moved it out of"
    );
}

#[test]
fn a_pointer_with_no_plugin_table_falls_back_to_the_legacy_file() {
    let fixture = ServiceFixture::new();
    create(&fixture, "Invisible", None);
    let dir = fixture.cwd().join(".storyhook");
    std::fs::create_dir_all(&dir).expect("creating the config directory");
    std::fs::write(dir.join("plugin-config.toml"), "enabled = false\n").expect("writing");
    std::fs::write(
        fixture.cwd().join(".storyhook.toml"),
        "schema = 1\nuuid = \"11111111-1111-4111-8111-111111111111\"\nprefix = \"SH\"\n",
    )
    .expect("writing the pointer");

    assert_eq!(
        SessionService::new(&fixture.ctx())
            .context()
            .expect("building"),
        "{}",
        "the two storage models coexist until the daemon wave; a repository that \
         has not moved its config must keep working"
    );
}
