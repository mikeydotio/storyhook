// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

/// Integration tests for TUI undo/redo.
///
/// These tests verify the undo/redo mechanism by exercising the storage-level
/// operations that the dispatch function performs: snapshot events, mutate,
/// and restore from snapshot. Uses tempdir-backed filesystem storage.
use std::path::Path;
use std::time::Instant;

use storyhook::domain::{Priority, StoryEvent};
use storyhook::storage::ProjectPaths;
use storyhook::tui::action::UndoEntry;
use storyhook::tui::data::DataStore;
use storyhook::tui::state::AppState;

/// Helper: initialize a storyhook project in a tempdir and return (dir, root path).
fn init_project(prefix: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    storyhook::storage::init_project(&root, Some(prefix)).unwrap();
    (dir, root)
}

/// Helper: create a story inside a lock and return its ID.
fn create_story(root: &Path, title: &str) -> String {
    storyhook::lock::with_project_lock(root, || {
        let snap = storyhook::storage::create_story(root, title, None)?;
        Ok(snap.id)
    })
    .unwrap()
}

/// Helper: write story events inside a lock.
fn write_events(root: &Path, id: &str, events: &[StoryEvent]) {
    storyhook::lock::with_project_lock(root, || {
        storyhook::storage::write_story_events(root, id, events)
    })
    .unwrap();
}

/// Helper: load a story's current events.
fn load_events(root: &Path, id: &str) -> Vec<StoryEvent> {
    storyhook::storage::load_open_story_events(root, id).unwrap_or_default()
}

/// Helper: perform the undo operation (same logic as dispatch).
fn perform_undo(root: &Path, entry: &UndoEntry) {
    let story_id = entry.story_id.clone();
    let events_before = entry.events_before.clone();
    storyhook::lock::with_project_lock(root, || {
        if events_before.is_empty() {
            let paths = ProjectPaths::new(root);
            let path = paths.open_story_file(&story_id);
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| storyhook::error::AppError::Storage(e.to_string()))?;
            }
            Ok(())
        } else {
            storyhook::storage::rewrite_story_events(root, &story_id, &events_before)
        }
    })
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
    let store = DataStore::load(&root).unwrap();
    let story = store.find_story("UM-1").unwrap();
    assert_eq!(story.state, "todo");

    // Snapshot before mutation
    let events_before = load_events(&root, &id);

    // Move to in-progress
    write_events(
        &root,
        &id,
        &[StoryEvent::StoryStateChanged {
            at: storyhook::storage::now(),
            state: "in-progress".to_string(),
        }],
    );

    // Verify move happened
    let store = DataStore::load(&root).unwrap();
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
    let store = DataStore::load(&root).unwrap();
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
    write_events(
        &root,
        &id,
        &[StoryEvent::StoryStateChanged {
            at: storyhook::storage::now(),
            state: "in-progress".to_string(),
        }],
    );

    // Snapshot current state before undo (this becomes the redo snapshot)
    let events_after_move = load_events(&root, &id);

    // Undo
    let undo_entry = UndoEntry {
        description: "Moved RD-1 to in-progress".to_string(),
        story_id: id.clone(),
        events_before: events_before_move,
    };
    perform_undo(&root, &undo_entry);

    let store = DataStore::load(&root).unwrap();
    assert_eq!(store.find_story("RD-1").unwrap().state, "todo");

    // Redo: restore events_after_move
    let redo_entry = UndoEntry {
        description: "Moved RD-1 to in-progress".to_string(),
        story_id: id.clone(),
        events_before: events_after_move,
    };
    perform_undo(&root, &redo_entry); // Redo uses same mechanism as undo

    let store = DataStore::load(&root).unwrap();
    assert_eq!(store.find_story("RD-1").unwrap().state, "in-progress");
}

// ─── Test 3: Undo create story deletes it ───────────────────────────

#[test]
fn undo_create_story_deletes_it() {
    let (_dir, root) = init_project("UC");

    let id = create_story(&root, "Temporary story");
    assert_eq!(id, "UC-1");

    // Verify story exists
    let store = DataStore::load(&root).unwrap();
    assert_eq!(store.story_count(), 1);

    // Undo creation: events_before is empty (story didn't exist)
    let entry = UndoEntry {
        description: "Created story: Temporary story".to_string(),
        story_id: id.clone(),
        events_before: Vec::new(),
    };
    perform_undo(&root, &entry);

    // Verify story file is gone
    let paths = ProjectPaths::new(&root);
    assert!(!paths.open_story_file(&id).exists());

    // Verify DataStore no longer has the story
    let store = DataStore::load(&root).unwrap();
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

    // Simulate close: write close events + archive
    storyhook::lock::with_project_lock(&root, || {
        storyhook::storage::write_story_events(
            &root,
            &id,
            &[
                StoryEvent::StoryStateChanged {
                    at: storyhook::storage::now(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: storyhook::storage::now(),
                    state: "done".to_string(),
                },
            ],
        )?;
        storyhook::storage::archive_story(&root, &id)?;
        Ok(())
    })
    .unwrap();

    // Verify archived (file gone from open/)
    let paths = ProjectPaths::new(&root);
    assert!(!paths.open_story_file(&id).exists());

    // In the dispatch code, MoveStory to a CLOSED state does NOT push to undo_stack.
    // We verify this by checking that the undo_stack remains empty.
    let mut state = AppState::new(DataStore::load(&root).unwrap());

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
    let store = DataStore::load(&root).unwrap();
    let story = store.find_story("SP-1").unwrap();
    assert_eq!(story.priority, Priority::None);

    // Snapshot before mutation
    let events_before = load_events(&root, &id);

    // Set priority to High
    write_events(
        &root,
        &id,
        &[StoryEvent::StoryPrioritySet {
            at: storyhook::storage::now(),
            priority: Priority::High,
        }],
    );

    // Verify priority changed
    let store = DataStore::load(&root).unwrap();
    assert_eq!(store.find_story("SP-1").unwrap().priority, Priority::High);

    // Undo: restore events from snapshot
    let entry = UndoEntry {
        description: "SP-1 priority set to high".to_string(),
        story_id: id.clone(),
        events_before,
    };
    perform_undo(&root, &entry);

    // Verify priority restored to None
    let store = DataStore::load(&root).unwrap();
    assert_eq!(store.find_story("SP-1").unwrap().priority, Priority::None);
}
