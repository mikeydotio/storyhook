//! `SessionService` — the JSON block an agent's session hook injects, and the
//! seam's history read.
//!
//! The envelope shape is a contract with Claude Code, not an internal detail:
//! the golden corpus pins its bytes, and a session hook that prints anything
//! else — an error, a diagnostic, a bare string — lands in a model's context.

use storyhook::cli::{HistoryAction, Invocation};
use storyhook::domain::StoryEvent;
use storyhook::invoke::dispatch;
use storyhook::output::Response;
use storyhook::service::{NewStoryInput, SessionService, StoryService};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture};

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

/// Regression test for SH-126 (council verdict, recorded on that story): a
/// story parked
/// in the literal `blocked` state, with no unmet `blocked-by` edge, used to
/// still count as ready and could be offered as the session's `Next:` pick
/// — `is_ready` never inspected `story.state`.
#[test]
fn a_story_in_the_blocked_state_is_never_the_next_line() {
    let fixture = ServiceFixture::new();
    let only = create(&fixture, "Manually blocked", None);
    StoryService::new(&fixture.ctx())
        .set_state(&only, "blocked", None, None, None)
        .expect("blocking");

    let context = context(&fixture);
    assert!(context.contains("1 open stories, 0 ready"), "{context}");
    assert!(!context.contains("Next:"), "{context}");
}

/// Regression test for SH-236: a story already moved to `in-progress` (the
/// project's configured active state) has already been claimed by someone,
/// so it must not still count as ready or be offered as the session's
/// `Next:` pick — `is_ready` never inspected `story.state` beyond the
/// required `blocked` slug, the same gap SH-126 closed for that one slug.
#[test]
fn a_story_already_in_progress_is_never_the_next_line() {
    let fixture = ServiceFixture::new();
    let claimed = create(&fixture, "Claimed elsewhere", None);
    StoryService::new(&fixture.ctx())
        .set_state(&claimed, "in-progress", None, None, None)
        .expect("claiming");

    let context = context(&fixture);
    assert!(context.contains("1 open stories, 0 ready"), "{context}");
    assert!(!context.contains("Next:"), "{context}");
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

/// The `Next:` line has to name whatever `story next` would offer first, and
/// the two used to share a comparator by copy-paste rather than by code —
/// `highest_priority`'s own doc comment said so. Two `high` stories tie on
/// priority (the fixture's clock is fixed, so they tie on the old
/// `created_at` key too), straddling the `SH-9`/`SH-10` boundary where a
/// lexicographic fallback (`"SH-10" < "SH-9"` as strings) and a numeric one
/// visibly disagree (SH-63).
#[test]
fn the_next_line_breaks_a_tie_by_story_number_not_by_id_string() {
    let fixture = ServiceFixture::new();
    for index in 1..=8 {
        create(&fixture, &format!("Filler {index}"), None);
    }
    let lower_numbered = create(&fixture, "Urgent A", Some("high")); // SH-9
    let _higher_numbered = create(&fixture, "Urgent B", Some("high")); // SH-10

    let context = context(&fixture);
    assert!(
        context.contains(&format!("Next: {lower_numbered} — Urgent A (high)")),
        "the lower-numbered tied story must win, not the one whose id string \
         sorts first (`SH-10` < `SH-9` as text): {context}"
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

// --- dispatch sentinel (SH-231) ---------------------------------------------

fn read_sentinel(fixture: &ServiceFixture) -> serde_json::Value {
    let raw = std::fs::read_to_string(fixture.cwd().join(".claude/dispatch-sentinel.json"))
        .expect("the sentinel should have been written");
    serde_json::from_str(&raw).expect("the sentinel should be valid JSON")
}

#[test]
fn dispatching_session_start_publishes_a_sentinel_beside_the_hook_envelope() {
    let fixture = ServiceFixture::new();
    let response = dispatch(&fixture.ctx(), Invocation::SessionStart).expect("dispatching");
    assert!(matches!(response, Response::RawJson(_)));

    let sentinel = read_sentinel(&fixture);
    assert_eq!(sentinel["protocol_version"], 1);
    assert_eq!(sentinel["written_at"], FIXTURE_NOW);
    assert_eq!(
        sentinel["story_id"],
        fixture
            .cwd()
            .file_name()
            .and_then(|n| n.to_str())
            .expect("the fixture cwd has a name"),
        "story_id is best-effort, derived from the dispatch worktree's own directory name"
    );
    assert!(
        sentinel["session_id"].is_null(),
        "no stdin was piped, so session_id stays absent rather than guessed: {sentinel}"
    );
}

#[test]
fn the_sentinels_session_id_comes_from_the_piped_hook_payload() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx().with_stdin(Some(
        r#"{"session_id":"sess-42","source":"startup"}"#.to_string(),
    ));
    SessionService::new(&ctx).publish_sentinel();

    let sentinel = read_sentinel(&fixture);
    assert_eq!(sentinel["session_id"], "sess-42");
}

#[test]
fn a_sentinel_write_failure_never_blanks_out_a_real_context_envelope() {
    let fixture = ServiceFixture::new();
    // `.claude` exists as a plain FILE, not a directory, so `create_dir_all`
    // inside `publish_sentinel` fails — this must degrade independently of
    // `context()`, which loaded real project state and has something to say.
    std::fs::write(fixture.cwd().join(".claude"), b"not a directory")
        .expect("seeding the collision");

    let ctx = fixture.ctx();
    let service = SessionService::new(&ctx);
    service.publish_sentinel(); // must not panic
    let raw = service.context().expect("context still builds");
    assert!(
        raw.contains("PROJECT STATE"),
        "a sentinel-write failure blanked out real project state: {raw}"
    );
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
fn restoring_a_history_goes_through_dispatch_and_answers_with_the_story() {
    // The action the port owed a design rather than a translation. It has one
    // now — a compensating write — so the arm dispatches like any other and
    // answers with the story as it stands afterwards.
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Cannot be rewound", None);
    let response = dispatch(
        &fixture.ctx(),
        Invocation::History {
            action: HistoryAction::Restore {
                id: id.clone(),
                events: Vec::new(),
            },
        },
    )
    .expect("restoring is a supported action");
    match response {
        Response::Story(view) => {
            assert_eq!(view.story.id, id);
            assert!(
                view.story.deleted,
                "restoring to an empty history deletes the story"
            );
        }
        other => panic!("expected the story back, got {other:?}"),
    }
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

// ---------------------------------------------------------------------------
// Undo, as compensating events
// ---------------------------------------------------------------------------

mod undo {
    use storyhook::domain::{StoryEvent, StorySnapshot};
    use storyhook::service::{
        FieldEdits, NewStoryInput, RelationService, StoryService, history, session,
    };
    use storyhook_test_support::ServiceFixture;

    /// A story, and the log it had at that moment — the TUI's undo snapshot.
    fn snapshot(fixture: &ServiceFixture, title: &str) -> (String, Vec<StoryEvent>) {
        let ctx = fixture.ctx();
        let id = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: title.to_string(),
                ..NewStoryInput::default()
            })
            .expect("creating a story")
            .id;
        let before = session::history(&ctx, &id).expect("reading the history");
        (id, before)
    }

    /// The story as it stands.
    fn show(fixture: &ServiceFixture, id: &str) -> StorySnapshot {
        let ctx = fixture.ctx();
        let events = session::history(&ctx, id).expect("reading the history");
        let states = {
            use storyhook::store::{ReadOps as _, Store as _};
            fixture
                .store()
                .read(|tx| tx.state_map(fixture.project()))
                .expect("reading the catalog")
        };
        storyhook::domain::fold_story(id, &events, &states).expect("folding")
    }

    /// Undoes back to `target`, returning the compensating events it appended.
    fn undo(fixture: &ServiceFixture, id: &str, target: &[StoryEvent]) -> Vec<StoryEvent> {
        history::restore(&fixture.ctx(), id, target).expect("restoring")
    }

    #[test]
    fn nothing_to_undo_writes_nothing() {
        let fixture = ServiceFixture::new();
        let (id, before) = snapshot(&fixture, "Unchanged");
        assert!(
            undo(&fixture, &id, &before).is_empty(),
            "restoring a story to what it already is must append no events"
        );
        assert_eq!(session::history(&fixture.ctx(), &id).unwrap(), before);
    }

    #[test]
    fn a_move_is_undone_by_moving_back() {
        let fixture = ServiceFixture::new();
        let (id, before) = snapshot(&fixture, "Moves around");
        StoryService::new(&fixture.ctx())
            .set_state(&id, "in-progress", None, None, None)
            .expect("moving");

        let compensation = undo(&fixture, &id, &before);
        assert!(matches!(
            compensation.as_slice(),
            [StoryEvent::StoryStateChanged { state, .. }] if state == "todo"
        ));
        assert_eq!(show(&fixture, &id).state, "todo");
        // Append-only: the move is still in the history, and so is its undo.
        assert_eq!(session::history(&fixture.ctx(), &id).unwrap().len(), 3);
    }

    #[test]
    fn a_comment_is_undone_by_retracting_it() {
        // Comments are the one part of a story that only accumulates, so this
        // is the inverse that needed a new event rather than a field set back.
        let fixture = ServiceFixture::new();
        let (id, before) = snapshot(&fixture, "Gets a comment");
        StoryService::new(&fixture.ctx())
            .comment(&id, "said in haste")
            .expect("commenting");
        assert_eq!(show(&fixture, &id).comments.len(), 1);

        let compensation = undo(&fixture, &id, &before);
        assert!(
            matches!(
                compensation.as_slice(),
                [StoryEvent::StoryCommentRetracted { text, .. }] if text == "said in haste"
            ),
            "{compensation:?}"
        );
        assert!(show(&fixture, &id).comments.is_empty());
        // The retraction names what it withdrew, so the audit trail still says
        // the comment was made.
        let history = session::history(&fixture.ctx(), &id).expect("history");
        assert!(history.iter().any(
            |e| matches!(e, StoryEvent::StoryCommentAdded { text, .. } if text == "said in haste")
        ));
    }

    #[test]
    fn an_assignment_is_undone_by_clearing_the_assignee() {
        // The sibling case: a field with an event that sets it and, until now,
        // none that cleared it.
        let fixture = ServiceFixture::new();
        fixture.add_member("ada", "Ada", None);
        let (id, before) = snapshot(&fixture, "Gets assigned");
        StoryService::new(&fixture.ctx())
            .assign(&id, "ada")
            .expect("assigning");
        assert_eq!(show(&fixture, &id).assignee.as_deref(), Some("ada"));

        let compensation = undo(&fixture, &id, &before);
        assert!(
            matches!(
                compensation.as_slice(),
                [StoryEvent::StoryAssigneeCleared { .. }]
            ),
            "{compensation:?}"
        );
        assert_eq!(show(&fixture, &id).assignee, None);
    }

    #[test]
    fn a_reassignment_is_undone_by_assigning_the_previous_member() {
        let fixture = ServiceFixture::new();
        fixture.add_member("ada", "Ada", None);
        fixture.add_member("grace", "Grace", None);
        let (id, _) = snapshot(&fixture, "Changes hands");
        StoryService::new(&fixture.ctx())
            .assign(&id, "ada")
            .expect("assigning");
        let before = session::history(&fixture.ctx(), &id).expect("history");
        StoryService::new(&fixture.ctx())
            .assign(&id, "grace")
            .expect("reassigning");

        undo(&fixture, &id, &before);
        assert_eq!(show(&fixture, &id).assignee.as_deref(), Some("ada"));
    }

    #[test]
    fn the_scalar_fields_are_each_set_back() {
        let fixture = ServiceFixture::new();
        let (id, before) = snapshot(&fixture, "Edited everywhere");
        StoryService::new(&fixture.ctx())
            .set_fields(
                &id,
                &FieldEdits {
                    title: Some("A new title".to_string()),
                    priority: Some("high".to_string()),
                    labels: Some("alpha,beta".to_string()),
                    description: Some("said something".to_string()),
                    blocked: Some("a decision".to_string()),
                    ..FieldEdits::default()
                },
            )
            .expect("editing");

        undo(&fixture, &id, &before);
        let after = show(&fixture, &id);
        assert_eq!(after.title, "Edited everywhere");
        assert_eq!(after.priority, storyhook::domain::Priority::None);
        assert!(after.labels.is_empty(), "{:?}", after.labels);
        assert_eq!(after.description.as_deref().unwrap_or(""), "");
        assert_eq!(after.awaiting, None);
    }

    #[test]
    fn a_relation_is_undone_on_both_ends() {
        // The one inverse that has to reach a *second* story. Writing only this
        // story's half would leave exactly the one-sided edge the schema and
        // the doctor exist to prevent.
        let fixture = ServiceFixture::new();
        let (a, before) = snapshot(&fixture, "One end");
        let (b, _) = snapshot(&fixture, "The other end");
        RelationService::new(&fixture.ctx())
            .relate(&a, "blocks", &b, false)
            .expect("relating");
        assert_eq!(show(&fixture, &b).relationships.len(), 1);

        undo(&fixture, &a, &before);
        assert!(show(&fixture, &a).relationships.is_empty());
        assert!(
            show(&fixture, &b).relationships.is_empty(),
            "the far end must lose the edge too, or the undo fabricates an SH-60 violation"
        );
    }

    #[test]
    fn undoing_a_creation_deletes_the_story_rather_than_erasing_it() {
        let fixture = ServiceFixture::new();
        let (id, _) = snapshot(&fixture, "Created in error");

        let compensation = undo(&fixture, &id, &[]);
        assert!(
            matches!(compensation.as_slice(), [StoryEvent::StoryDeleted { .. }]),
            "{compensation:?}"
        );
        let after = show(&fixture, &id);
        assert!(after.deleted);
        assert!(
            !session::history(&fixture.ctx(), &id).unwrap().is_empty(),
            "the id stays spent and the history survives — an id that can vanish is \
             an id that cannot be quoted anywhere"
        );
    }

    #[test]
    fn an_undo_can_itself_be_undone() {
        // Redo, from the TUI's point of view: it is the same invocation with
        // the two logs the other way round.
        let fixture = ServiceFixture::new();
        let (id, before) = snapshot(&fixture, "Back and forth");
        StoryService::new(&fixture.ctx())
            .set_state(&id, "in-progress", None, None, None)
            .expect("moving");
        let after_move = session::history(&fixture.ctx(), &id).expect("history");

        undo(&fixture, &id, &before);
        assert_eq!(show(&fixture, &id).state, "todo");
        undo(&fixture, &id, &after_move);
        assert_eq!(show(&fixture, &id).state, "in-progress");
    }
}
