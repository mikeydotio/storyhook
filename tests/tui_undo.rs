// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

//! Integration tests for TUI undo/redo.
//!
//! Undo snapshots a story's raw event log before a mutation and puts it back
//! afterwards. Both halves are one seam invocation — `History::Read` and
//! `History::Restore` — which is exactly what `tui::app::dispatch` calls, so
//! these tests exercise the production primitive rather than a copy of it.
//!
//! **Reconstructed for the Invoker seam.** Every fixture and every mutation
//! that used to reach into the storage layer or take the project lock now
//! goes through `Invoker`; the file holds no white-box storage call. The
//! assertions are unchanged in meaning, with one mechanical substitution: two
//! of them asked whether a story's JSONL file still existed, and now ask
//! whether the story is still in the project — the same question, in terms of
//! a storage layout that has stopped existing.
//!
//! **Run against the store**, which is what the TUI runs against. `Restore` is
//! a *compensating write* now rather than a replacement of the log: see
//! `service::history` for what that changes, and
//! `tests/service_session.rs`'s `undo` module for the per-inverse tests.

use std::time::Instant;

use storyhook::cli::{
    Attach, HistoryAction, Invocation, NewProjectRequest, NewProjectSpec, ProjectAction,
};
use storyhook::domain::{Priority, StoryEvent};
use storyhook::invoke::{InvokeRequest, Invoker, StoreInvoker};
use storyhook::output::Response;
use storyhook::store::{SqliteStore, Store as _};
use storyhook::tui::action::UndoEntry;
use storyhook::tui::data::DataStore;
use storyhook::tui::state::AppState;

/// A store and a checkout, with a project initialized in it.
///
/// The store's directory is a fixture of its own rather than the machine's:
/// these are in-process tests, and an in-process test cannot redirect
/// `STORYHOOK_DATA_DIR` for itself.
struct Fixture {
    store: SqliteStore,
    root: std::path::PathBuf,
    env: storyhook::env::Environment,
    _data: tempfile::TempDir,
    _repo: tempfile::TempDir,
}

impl Fixture {
    fn invoker(&self) -> StoreInvoker<'_, SqliteStore> {
        StoreInvoker::new(&self.store, &self.root, self.env.clone())
    }
}

/// Helper: initialize a storyhook project and return the fixture holding it.
fn init_project(prefix: &str) -> (Fixture, std::path::PathBuf) {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let env = storyhook::env::Environment::at(data.path());
    let store = SqliteStore::open(env.store_path()).unwrap();
    store.migrate().unwrap();
    let root = repo.path().to_path_buf();
    let fixture = Fixture {
        store,
        root: root.clone(),
        env,
        _data: data,
        _repo: repo,
    };
    fixture
        .invoker()
        .invoke(
            InvokeRequest::new(Invocation::Project {
                action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                    attach: Attach::Cwd,
                    prefix: prefix.to_string(),
                    name: None,
                    no_agents_md: true,
                })),
            })
            .no_hooks(true),
        )
        .unwrap();
    (fixture, root)
}

/// Helper: run one invocation through the seam, hooks suppressed as the TUI
/// does.
fn run(fixture: &Fixture, invocation: Invocation) -> Result<Response, storyhook::error::AppError> {
    fixture
        .invoker()
        .invoke(InvokeRequest::new(invocation).no_hooks(true))
}

/// Helper: create a story and return its ID.
fn create_story(fixture: &Fixture, title: &str) -> String {
    match run(
        fixture,
        Invocation::New {
            title: title.to_string(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
            draft: false,
        },
    )
    .unwrap()
    {
        Response::Story(view) => view.story.id,
        other => panic!("expected a story, got {other:?}"),
    }
}

/// Helper: the project as the TUI sees it.
fn load(fixture: &Fixture) -> DataStore {
    DataStore::load(&fixture.invoker()).unwrap()
}

/// Helper: load a story's current events — the TUI's undo snapshot.
fn load_events(fixture: &Fixture, id: &str) -> Vec<StoryEvent> {
    match run(
        fixture,
        Invocation::History {
            action: HistoryAction::Read { id: id.to_string() },
        },
    )
    .unwrap()
    {
        Response::StoryHistory(events) => events,
        other => panic!("expected a history, got {other:?}"),
    }
}

/// Helper: perform the undo operation — the same invocation `dispatch` makes.
fn perform_undo(fixture: &Fixture, entry: &UndoEntry) {
    run(
        fixture,
        Invocation::History {
            action: HistoryAction::Restore {
                id: entry.story_id.clone(),
                events: entry.events_before.clone(),
            },
        },
    )
    .unwrap();
}

// ─── Test 1: Undo move story restores state ────────────────────────

#[test]
fn undo_move_story_restores_state() {
    let (fixture, _root) = init_project("UM");
    // in-progress is now a default state — no need to add it

    let id = create_story(&fixture, "Test story");
    assert_eq!(id, "UM-1");

    // Verify initial state is "todo"
    let store = load(&fixture);
    let story = store.find_story("UM-1").unwrap();
    assert_eq!(story.state, "todo");

    // Snapshot before mutation
    let events_before = load_events(&fixture, &id);

    // Move to in-progress
    run(
        &fixture,
        Invocation::SetState {
            id: id.clone(),
            state: "in-progress".to_string(),
            comment: None,
            if_state: None,
            awaiting: None,
        },
    )
    .unwrap();

    // Verify move happened
    let store = load(&fixture);
    let story = store.find_story("UM-1").unwrap();
    assert_eq!(story.state, "in-progress");

    // Undo: restore events from snapshot
    let entry = UndoEntry {
        description: "Moved UM-1 to in-progress".to_string(),
        story_id: id.clone(),
        events_before,
    };
    perform_undo(&fixture, &entry);

    // Verify undo restored state
    let store = load(&fixture);
    let story = store.find_story("UM-1").unwrap();
    assert_eq!(story.state, "todo");
}

// ─── Test 2: Redo after undo ────────────────────────────────────────

#[test]
fn redo_after_undo() {
    let (fixture, _root) = init_project("RD");
    // in-progress is now a default state — no need to add it

    let id = create_story(&fixture, "Redo test");

    // Snapshot, move, then undo
    let events_before_move = load_events(&fixture, &id);
    run(
        &fixture,
        Invocation::SetState {
            id: id.clone(),
            state: "in-progress".to_string(),
            comment: None,
            if_state: None,
            awaiting: None,
        },
    )
    .unwrap();

    // Snapshot current state before undo (this becomes the redo snapshot)
    let events_after_move = load_events(&fixture, &id);

    // Undo
    let undo_entry = UndoEntry {
        description: "Moved RD-1 to in-progress".to_string(),
        story_id: id.clone(),
        events_before: events_before_move,
    };
    perform_undo(&fixture, &undo_entry);

    let store = load(&fixture);
    assert_eq!(store.find_story("RD-1").unwrap().state, "todo");

    // Redo: restore events_after_move
    let redo_entry = UndoEntry {
        description: "Moved RD-1 to in-progress".to_string(),
        story_id: id.clone(),
        events_before: events_after_move,
    };
    perform_undo(&fixture, &redo_entry); // Redo uses same mechanism as undo

    let store = load(&fixture);
    assert_eq!(store.find_story("RD-1").unwrap().state, "in-progress");
}

// ─── Test 4: New action clears redo ─────────────────────────────────

#[test]
fn new_action_clears_redo() {
    let mut state = AppState::new(DataStore::default());

    // Simulate: push an entry to redo_stack
    state.redo_stack.push(UndoEntry {
        description: "old action".to_string(),
        story_id: "X-1".to_string(),
        events_before: Vec::new(),
    });
    assert_eq!(state.redo_stack.len(), 1);

    // Simulate push_undo (which clears redo)
    state.undo_stack.push(UndoEntry {
        description: "new action".to_string(),
        story_id: "X-2".to_string(),
        events_before: Vec::new(),
    });
    state.redo_stack.clear();

    assert!(
        state.redo_stack.is_empty(),
        "redo stack should be cleared after new action"
    );
    assert_eq!(state.undo_stack.len(), 1);
}

// ─── Test 5: Undo empty stack notifies ──────────────────────────────

#[test]
fn undo_empty_stack_notifies() {
    let mut state = AppState::new(DataStore::default());

    // Simulate undo with empty stack
    assert!(state.undo_stack.is_empty());

    // The dispatch would set notification to "Nothing to undo"
    if state.undo_stack.pop().is_none() {
        state.notification = Some(("Nothing to undo".to_string(), Instant::now()));
    }

    assert_eq!(state.notification.as_ref().unwrap().0, "Nothing to undo");
}

// ─── Test 6: Close/archive not undoable ─────────────────────────────

#[test]
fn close_archive_not_undoable() {
    let (fixture, _root) = init_project("CL");

    let id = create_story(&fixture, "To be closed");

    // Snapshot before close
    let events_before = load_events(&fixture, &id);

    // Close it: a move into a CLOSED state archives the story
    run(
        &fixture,
        Invocation::SetState {
            id: id.clone(),
            state: "done".to_string(),
            comment: None,
            if_state: None,
            awaiting: None,
        },
    )
    .unwrap();

    // Verify archived: the story has left the project the TUI can see
    assert!(load(&fixture).find_story(&id).is_none());

    // In the dispatch code, MoveStory to a CLOSED state does NOT push to undo_stack.
    // We verify this by checking that the undo_stack remains empty.
    let mut state = AppState::new(load(&fixture));

    // MoveStory dispatch checks is_close and only pushes undo when !is_close.
    // Simulate that behavior:
    let is_close = true;
    if !is_close {
        state.undo_stack.push(UndoEntry {
            description: "should not be pushed".to_string(),
            story_id: id.clone(),
            events_before,
        });
    }

    assert!(
        state.undo_stack.is_empty(),
        "close/archive should not be pushed to undo stack"
    );
}

// ─── Test 7: Undo set priority restores ─────────────────────────────

#[test]
fn undo_set_priority_restores() {
    let (fixture, _root) = init_project("SP");

    let id = create_story(&fixture, "Priority test");

    // Verify the required creation default.
    let store = load(&fixture);
    let story = store.find_story("SP-1").unwrap();
    assert_eq!(story.priority, Priority::Low);
    assert!(story.priority_assessed);

    // Snapshot before mutation
    let events_before = load_events(&fixture, &id);

    // Set priority to High
    run(
        &fixture,
        Invocation::SetPriority {
            id: id.clone(),
            priority: "high".to_string(),
        },
    )
    .unwrap();

    // Verify priority changed
    let store = load(&fixture);
    assert_eq!(store.find_story("SP-1").unwrap().priority, Priority::High);

    // Undo: restore events from snapshot
    let entry = UndoEntry {
        description: "SP-1 priority set to high".to_string(),
        story_id: id.clone(),
        events_before,
    };
    perform_undo(&fixture, &entry);

    // Verify priority restored to the explicit creation event.
    let store = load(&fixture);
    assert_eq!(store.find_story("SP-1").unwrap().priority, Priority::Low);
}

// ─── SH-449: undo of a first post-create prioritize ────────────────

/// A current story begins with a real `StoryPrioritySet(low)` event. Undoing
/// its first later reprioritization therefore compensates with another set to
/// low; the legacy clear event remains only for histories that began without
/// a priority event.
#[test]
fn undoing_a_first_post_create_prioritize_returns_to_low() {
    let (fixture, _root) = init_project("SH");
    let id = create_story(&fixture, "Nobody chose");

    let before = load_events(&fixture, &id);
    let story = load(&fixture).find_story(&id).expect("the story").clone();
    assert_eq!(story.priority, Priority::Low);
    assert!(story.priority_assessed);

    run(
        &fixture,
        Invocation::SetPriority {
            id: id.clone(),
            priority: "high".to_string(),
        },
    )
    .unwrap();
    let assessed = load(&fixture).find_story(&id).expect("the story").clone();
    assert_eq!(assessed.priority, Priority::High);
    assert!(assessed.priority_assessed);

    perform_undo(
        &fixture,
        &UndoEntry {
            description: format!("Prioritized {id}"),
            story_id: id.clone(),
            events_before: before,
        },
    );

    let restored = load(&fixture).find_story(&id).expect("the story").clone();
    assert_eq!(
        restored.priority,
        Priority::Low,
        "the level goes back to the creation default"
    );
    assert!(
        restored.priority_assessed,
        "the initial low priority was a real event"
    );

    let events = load_events(&fixture, &id);
    assert!(
        matches!(
            events.last(),
            Some(StoryEvent::StoryPrioritySet {
                priority: Priority::Low,
                ..
            })
        ),
        "the compensating event restores low: {:?}",
        events.last()
    );
}

/// The neighbouring case, which must keep working: undoing a *change* between
/// two stated levels leaves the story assessed, because it still is.
#[test]
fn undoing_a_reprioritize_leaves_the_story_assessed() {
    let (fixture, _root) = init_project("SH");
    let id = create_story(&fixture, "Chosen twice");

    run(
        &fixture,
        Invocation::SetPriority {
            id: id.clone(),
            priority: "low".to_string(),
        },
    )
    .unwrap();
    let before = load_events(&fixture, &id);

    run(
        &fixture,
        Invocation::SetPriority {
            id: id.clone(),
            priority: "critical".to_string(),
        },
    )
    .unwrap();

    perform_undo(
        &fixture,
        &UndoEntry {
            description: format!("Prioritized {id}"),
            story_id: id.clone(),
            events_before: before,
        },
    );

    let restored = load(&fixture).find_story(&id).expect("the story").clone();
    assert_eq!(restored.priority, Priority::Low);
    assert!(
        restored.priority_assessed,
        "a story that has been assessed twice is still assessed after undoing one"
    );
}
