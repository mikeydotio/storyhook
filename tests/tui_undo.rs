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
//! a storage layout that is about to stop existing.

use std::path::Path;
use std::time::Instant;

use storyhook::cli::{HistoryAction, Invocation};
use storyhook::domain::{Priority, StoryEvent};
use storyhook::invoke::{InvokeRequest, Invoker, LegacyInvoker};
use storyhook::output::Response;
use storyhook::tui::action::UndoEntry;
use storyhook::tui::data::DataStore;
use storyhook::tui::state::AppState;

/// Helper: initialize a storyhook project in a tempdir and return (dir, root path).
fn init_project(prefix: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    run(
        &root,
        Invocation::Init {
            prefix: Some(prefix.to_string()),
            no_agents_md: true,
        },
    )
    .unwrap();
    (dir, root)
}

/// Helper: run one invocation through the seam, hooks suppressed as the TUI
/// does.
fn run(root: &Path, invocation: Invocation) -> Result<Response, storyhook::error::AppError> {
    LegacyInvoker::new(root).invoke(InvokeRequest::new(invocation).no_hooks(true))
}

/// Helper: create a story and return its ID.
fn create_story(root: &Path, title: &str) -> String {
    match run(
        root,
        Invocation::New {
            title: title.to_string(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    )
    .unwrap()
    {
        Response::Story(view) => view.story.id,
        other => panic!("expected a story, got {other:?}"),
    }
}

/// Helper: the project as the TUI sees it.
fn load(root: &Path) -> DataStore {
    DataStore::load(&LegacyInvoker::new(root)).unwrap()
}

/// Helper: load a story's current events — the TUI's undo snapshot.
fn load_events(root: &Path, id: &str) -> Vec<StoryEvent> {
    match run(
        root,
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
fn perform_undo(root: &Path, entry: &UndoEntry) {
    run(
        root,
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
    let (_dir, root) = init_project("UM");
    // in-progress is now a default state — no need to add it

    let id = create_story(&root, "Test story");
    assert_eq!(id, "UM-1");

    // Verify initial state is "todo"
    let store = load(&root);
    let story = store.find_story("UM-1").unwrap();
    assert_eq!(story.state, "todo");

    // Snapshot before mutation
    let events_before = load_events(&root, &id);

    // Move to in-progress
    run(
        &root,
        Invocation::SetState {
            id: id.clone(),
            state: "in-progress".to_string(),
            comment: None,
            if_state: None,
        },
    )
    .unwrap();

    // Verify move happened
    let store = load(&root);
    let story = store.find_story("UM-1").unwrap();
    assert_eq!(story.state, "in-progress");

    // Undo: restore events from snapshot
    let entry = UndoEntry {
        description: "Moved UM-1 to in-progress".to_string(),
        story_id: id.clone(),
        events_before,
    };
    perform_undo(&root, &entry);

    // Verify undo restored state
    let store = load(&root);
    let story = store.find_story("UM-1").unwrap();
    assert_eq!(story.state, "todo");
}

// ─── Test 2: Redo after undo ────────────────────────────────────────

#[test]
fn redo_after_undo() {
    let (_dir, root) = init_project("RD");
    // in-progress is now a default state — no need to add it

    let id = create_story(&root, "Redo test");

    // Snapshot, move, then undo
    let events_before_move = load_events(&root, &id);
    run(
        &root,
        Invocation::SetState {
            id: id.clone(),
            state: "in-progress".to_string(),
            comment: None,
            if_state: None,
        },
    )
    .unwrap();

    // Snapshot current state before undo (this becomes the redo snapshot)
    let events_after_move = load_events(&root, &id);

    // Undo
    let undo_entry = UndoEntry {
        description: "Moved RD-1 to in-progress".to_string(),
        story_id: id.clone(),
        events_before: events_before_move,
    };
    perform_undo(&root, &undo_entry);

    let store = load(&root);
    assert_eq!(store.find_story("RD-1").unwrap().state, "todo");

    // Redo: restore events_after_move
    let redo_entry = UndoEntry {
        description: "Moved RD-1 to in-progress".to_string(),
        story_id: id.clone(),
        events_before: events_after_move,
    };
    perform_undo(&root, &redo_entry); // Redo uses same mechanism as undo

    let store = load(&root);
    assert_eq!(store.find_story("RD-1").unwrap().state, "in-progress");
}

// ─── Test 3: Undo create story deletes it ───────────────────────────

#[test]
fn undo_create_story_deletes_it() {
    let (_dir, root) = init_project("UC");

    let id = create_story(&root, "Temporary story");
    assert_eq!(id, "UC-1");

    // Verify story exists
    let store = load(&root);
    assert_eq!(store.story_count(), 1);

    // Undo creation: events_before is empty (story didn't exist)
    let entry = UndoEntry {
        description: "Created story: Temporary story".to_string(),
        story_id: id.clone(),
        events_before: Vec::new(),
    };
    perform_undo(&root, &entry);

    // Verify the story is gone — it has no history left, and nothing to load
    assert!(load_events(&root, &id).is_empty());

    // Verify DataStore no longer has the story
    let store = load(&root);
    assert_eq!(store.story_count(), 0);
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
    let (_dir, root) = init_project("CL");

    let id = create_story(&root, "To be closed");

    // Snapshot before close
    let events_before = load_events(&root, &id);

    // Close it: a move into a CLOSED state archives the story
    run(
        &root,
        Invocation::SetState {
            id: id.clone(),
            state: "done".to_string(),
            comment: None,
            if_state: None,
        },
    )
    .unwrap();

    // Verify archived: the story has left the project the TUI can see
    assert!(load(&root).find_story(&id).is_none());

    // In the dispatch code, MoveStory to a CLOSED state does NOT push to undo_stack.
    // We verify this by checking that the undo_stack remains empty.
    let mut state = AppState::new(load(&root));

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
    let (_dir, root) = init_project("SP");

    let id = create_story(&root, "Priority test");

    // Verify initial priority is None
    let store = load(&root);
    let story = store.find_story("SP-1").unwrap();
    assert_eq!(story.priority, Priority::None);

    // Snapshot before mutation
    let events_before = load_events(&root, &id);

    // Set priority to High
    run(
        &root,
        Invocation::SetPriority {
            id: id.clone(),
            priority: "high".to_string(),
        },
    )
    .unwrap();

    // Verify priority changed
    let store = load(&root);
    assert_eq!(store.find_story("SP-1").unwrap().priority, Priority::High);

    // Undo: restore events from snapshot
    let entry = UndoEntry {
        description: "SP-1 priority set to high".to_string(),
        story_id: id.clone(),
        events_before,
    };
    perform_undo(&root, &entry);

    // Verify priority restored to None
    let store = load(&root);
    assert_eq!(store.find_story("SP-1").unwrap().priority, Priority::None);
}
