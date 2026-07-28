// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

//! Integration tests for the TUI data path.
//!
//! These tests exercise DataStore + AppState + Board + Action dispatch
//! WITHOUT launching a terminal, verifying the full data flow from creation
//! through filtering and display.
//!
//! **Reconstructed for the Invoker seam.** Fixtures used to be built by
//! calling the storage layer under the project lock and by writing events
//! directly; they are now built out of the same `Invocation`s the TUI itself
//! issues, and `DataStore::load` takes an `Invoker`. No white-box storage call
//! is left. The assertions are unchanged — where a fixture could not be
//! expressed through the seam at all, the file still writes to `.storyhook/`
//! directly, because those cases fabricate states no API can produce and that
//! is precisely what they are for.

use std::path::Path;
use std::time::Instant;

use storyhook::cli::{Invocation, StateAction};
use storyhook::domain::{Priority, StateDef, SuperState};
use storyhook::error::AppError;
use storyhook::invoke::{InvokeRequest, Invoker, LegacyInvoker};
use storyhook::output::Response;
use storyhook::tui::action::{FilterSpec, View};
use storyhook::tui::components::board::{Board, RowItem};
use storyhook::tui::data::DataStore;
use storyhook::tui::focus::{FocusStack, FocusTarget, Modal};
use storyhook::tui::state::AppState;

/// Helper: run one invocation through the seam, hooks suppressed as the TUI
/// does.
fn run(root: &Path, invocation: Invocation) -> Result<Response, AppError> {
    LegacyInvoker::new(root).invoke(InvokeRequest::new(invocation).no_hooks(true))
}

/// Helper: the project as the TUI sees it — one `ProjectSnapshot`.
fn load(root: &Path) -> Result<DataStore, AppError> {
    DataStore::load(&LegacyInvoker::new(root))
}

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

/// Helper: add a member so `story assign` has someone to resolve to.
fn add_member(root: &Path, handle: &str) {
    run(
        root,
        Invocation::MemberAdd {
            input: storyhook::cli::MemberInput::Github(handle.to_string()),
        },
    )
    .unwrap();
}

/// Helper: move a story into a state.
fn set_state(root: &Path, id: &str, state: &str) {
    run(
        root,
        Invocation::SetState {
            id: id.to_string(),
            state: state.to_string(),
            comment: None,
            if_state: None,
        },
    )
    .unwrap();
}

/// Helper: set a story's labels, which for a story that has none is an add.
fn set_labels(root: &Path, id: &str, labels: &[&str]) {
    run(
        root,
        Invocation::SetLabels {
            id: id.to_string(),
            add: labels.iter().map(|l| (*l).to_string()).collect(),
            remove: Vec::new(),
        },
    )
    .unwrap();
}

/// Helper: set a story's priority.
fn set_priority(root: &Path, id: &str, priority: &str) {
    run(
        root,
        Invocation::SetPriority {
            id: id.to_string(),
            priority: priority.to_string(),
        },
    )
    .unwrap();
}

// ─── Test 1: Empty project ──────────────────────────────────────────

#[test]
fn empty_project_loads_with_no_stories() {
    let (_dir, root) = init_project("TP");
    let store = load(&root).unwrap();

    assert_eq!(store.story_count(), 0);
    assert_eq!(store.prefix, "TP");

    // Board should show section headers with zero counts
    let state = AppState::new(store);
    let board = Board::new();
    let rows = board.build_visible_rows(&state);

    // Default states: "todo" (open) and "done" (closed)
    // Only open states appear on the board
    assert!(
        rows.iter()
            .all(|r| matches!(r, RowItem::SectionHeader { count: 0, .. })),
        "all sections should be empty"
    );
    // At least one section header should exist (the default "todo" state)
    assert!(!rows.is_empty(), "should have at least one section header");
}

// ─── Test 2: Create + Edit flow ─────────────────────────────────────

#[test]
fn create_and_edit_story_flow() {
    let (_dir, root) = init_project("CE");

    // Create a story
    let id = create_story(&root, "Initial title");
    assert_eq!(id, "CE-1");

    // Verify it appears in DataStore
    let store = load(&root).unwrap();
    assert_eq!(store.story_count(), 1);
    let story = store.find_story("CE-1").unwrap();
    assert_eq!(story.title, "Initial title");
    assert_eq!(story.priority, Priority::None);
    assert!(story.assignee.is_none());
    assert!(story.comments.is_empty());

    // Set priority
    set_priority(&root, "CE-1", "high");
    let store = load(&root).unwrap();
    assert_eq!(store.find_story("CE-1").unwrap().priority, Priority::High);

    // Assign
    add_member(&root, "mikey");
    run(
        &root,
        Invocation::Assign {
            id: "CE-1".to_string(),
            member: "mikey".to_string(),
        },
    )
    .unwrap();
    let store = load(&root).unwrap();
    assert_eq!(
        store.find_story("CE-1").unwrap().assignee.as_deref(),
        Some("mikey")
    );

    // Add comment
    run(
        &root,
        Invocation::Comment {
            id: "CE-1".to_string(),
            text: "Work in progress".to_string(),
        },
    )
    .unwrap();
    let store = load(&root).unwrap();
    assert_eq!(store.find_story("CE-1").unwrap().comments.len(), 1);
    assert_eq!(
        store.find_story("CE-1").unwrap().comments[0].text,
        "Work in progress"
    );
}

// ─── Test 3: Move story through states ──────────────────────────────

#[test]
fn move_story_through_states() {
    let (_dir, root) = init_project("MV");

    // `in-progress` is a default state, so the board already has a second
    // OPEN column to move into.
    let id = create_story(&root, "Move me");

    // Story starts in "todo" (default open state)
    let store = load(&root).unwrap();
    assert_eq!(store.find_story(&id).unwrap().state, "todo");

    // Move to in-progress
    set_state(&root, &id, "in-progress");
    let store = load(&root).unwrap();
    assert_eq!(store.find_story(&id).unwrap().state, "in-progress");

    // Verify board groups it correctly
    let state = AppState::new(store);
    let board = Board::new();
    let rows = board.build_visible_rows(&state);

    // Find the story row and check it's under in-progress
    let mut found_in_progress_section = false;
    let mut found_story_after_section = false;
    for row in &rows {
        match row {
            RowItem::SectionHeader { slug, .. } if slug == "in-progress" => {
                found_in_progress_section = true;
            }
            RowItem::StoryRow { id: row_id } if found_in_progress_section && row_id == &id => {
                found_story_after_section = true;
                break;
            }
            RowItem::SectionHeader { .. } if found_in_progress_section => {
                // Hit another section header before finding the story
                break;
            }
            _ => {}
        }
    }
    assert!(
        found_story_after_section,
        "story should appear under in-progress section"
    );
}

// ─── Test 4: Move to CLOSED state archives the story ────────────────

#[test]
fn move_to_closed_state_archives_story() {
    let (_dir, root) = init_project("CL");
    let id = create_story(&root, "Close me");

    // Story exists in open snapshots
    let store = load(&root).unwrap();
    assert_eq!(store.story_count(), 1);

    // Move to "done" (closed state) - archive it
    set_state(&root, &id, "done");

    // Story should be gone from open snapshots
    let store = load(&root).unwrap();
    assert_eq!(store.story_count(), 0);
    assert!(store.find_story(&id).is_none());
}

// ─── Test 5: Filter application ─────────────────────────────────────

#[test]
fn filter_narrows_visible_stories() {
    let (_dir, root) = init_project("FL");

    let id1 = create_story(&root, "Fix login bug");
    let id2 = create_story(&root, "Add search feature");
    let id3 = create_story(&root, "Refactor database");

    // Set different priorities and labels
    add_member(&root, "mikey");
    set_priority(&root, &id1, "high");
    set_labels(&root, &id1, &["bug"]);
    set_priority(&root, &id2, "medium");
    set_labels(&root, &id3, &["tech-debt"]);
    run(
        &root,
        Invocation::Assign {
            id: id3.clone(),
            member: "mikey".to_string(),
        },
    )
    .unwrap();

    let store = load(&root).unwrap();
    assert_eq!(store.story_count(), 3);

    // Filter by text
    let mut state = AppState::new(store);
    state.filters.push(FilterSpec {
        text: Some("login".to_string()),
        ..Default::default()
    });
    let board = Board::new();
    let rows = board.build_visible_rows(&state);
    let story_rows: Vec<_> = rows
        .iter()
        .filter(|r| matches!(r, RowItem::StoryRow { .. }))
        .collect();
    assert_eq!(story_rows.len(), 1);

    // Filter by priority
    let store = load(&root).unwrap();
    let mut state = AppState::new(store);
    state.filters.push(FilterSpec {
        priority: Some(Priority::High),
        ..Default::default()
    });
    let rows = board.build_visible_rows(&state);
    let story_rows: Vec<_> = rows
        .iter()
        .filter(|r| matches!(r, RowItem::StoryRow { .. }))
        .collect();
    assert_eq!(story_rows.len(), 1);

    // Filter by label
    let store = load(&root).unwrap();
    let mut state = AppState::new(store);
    state.filters.push(FilterSpec {
        label: Some("bug".to_string()),
        ..Default::default()
    });
    let rows = board.build_visible_rows(&state);
    let story_rows: Vec<_> = rows
        .iter()
        .filter(|r| matches!(r, RowItem::StoryRow { .. }))
        .collect();
    assert_eq!(story_rows.len(), 1);

    // Combined filters (priority + assignee) should AND together
    let store = load(&root).unwrap();
    let mut state = AppState::new(store);
    state.filters.push(FilterSpec {
        assignee: Some("mikey".to_string()),
        ..Default::default()
    });
    state.filters.push(FilterSpec {
        label: Some("tech-debt".to_string()),
        ..Default::default()
    });
    let rows = board.build_visible_rows(&state);
    let story_rows: Vec<_> = rows
        .iter()
        .filter(|r| matches!(r, RowItem::StoryRow { .. }))
        .collect();
    assert_eq!(story_rows.len(), 1);
    if let RowItem::StoryRow { id } = story_rows[0] {
        assert_eq!(id, &id3);
    }
}

// ─── Test 6: Focus stack push/pop ───────────────────────────────────

#[test]
fn focus_stack_manages_modals_correctly() {
    let mut focus = FocusStack::new(FocusTarget::Board);
    assert!(!focus.has_modal());

    // Push story detail
    focus.push_modal(Modal::StoryDetail {
        story_id: "SH-1".to_string(),
    });
    assert!(focus.has_modal());
    assert_eq!(
        focus.top_modal(),
        Some(&Modal::StoryDetail {
            story_id: "SH-1".to_string()
        })
    );

    // Push help on top
    focus.push_modal(Modal::Help);
    assert_eq!(focus.top_modal(), Some(&Modal::Help));

    // Pop help, story detail is still there
    let popped = focus.pop_modal();
    assert_eq!(popped, Some(Modal::Help));
    assert_eq!(
        focus.top_modal(),
        Some(&Modal::StoryDetail {
            story_id: "SH-1".to_string()
        })
    );

    // Pop story detail
    focus.pop_modal();
    assert!(!focus.has_modal());

    // Base target is preserved
    assert_eq!(focus.base(), &FocusTarget::Board);
}

// ─── Test 7: Stale modal detection ─────────────────────────────────

#[test]
fn stale_modal_detected_after_story_archived() {
    let (_dir, root) = init_project("SM");
    let id = create_story(&root, "Will be archived");

    let store = load(&root).unwrap();
    let mut state = AppState::new(store);

    // Simulate opening the detail modal
    state.focus.push_modal(Modal::StoryDetail {
        story_id: id.clone(),
    });
    assert!(state.focus.has_modal());

    // Now archive the story externally (simulating CLI action)
    set_state(&root, &id, "done");

    // Refresh data (simulates what RefreshData action does)
    let new_data = load(&root).unwrap();
    state.data = new_data;

    // Check stale modal: story is gone, modal should be closed
    if let Some(Modal::StoryDetail { story_id }) = state.focus.top_modal()
        && state.data.find_story(story_id).is_none()
    {
        let stale_id = story_id.clone();
        state.focus.pop_modal();
        state.notification = Some((format!("Story {stale_id} no longer open"), Instant::now()));
    }

    assert!(!state.focus.has_modal(), "stale modal should be closed");
    assert!(state.notification.is_some(), "notification should be set");
    assert!(
        state
            .notification
            .as_ref()
            .unwrap()
            .0
            .contains("no longer open"),
        "notification should mention the story is gone"
    );
}

// ─── Test 8: Board section collapse ─────────────────────────────────

#[test]
fn collapsed_sections_hide_story_rows() {
    let (_dir, root) = init_project("CS");
    create_story(&root, "Story A");
    create_story(&root, "Story B");

    let store = load(&root).unwrap();
    let state = AppState::new(store);

    let mut board = Board::new();
    let rows = board.build_visible_rows(&state);
    // With todo expanded, we should see header + 2 story rows
    let story_count_expanded = rows
        .iter()
        .filter(|r| matches!(r, RowItem::StoryRow { .. }))
        .count();
    assert_eq!(story_count_expanded, 2);

    // Collapse the "todo" section
    board.collapsed.insert("todo".to_string());
    let rows = board.build_visible_rows(&state);
    let story_count_collapsed = rows
        .iter()
        .filter(|r| matches!(r, RowItem::StoryRow { .. }))
        .count();
    assert_eq!(story_count_collapsed, 0);

    // The section header should still be there
    let header = rows
        .iter()
        .find(|r| matches!(r, RowItem::SectionHeader { slug, .. } if slug == "todo"));
    assert!(header.is_some());
    if let Some(RowItem::SectionHeader {
        count, expanded, ..
    }) = header
    {
        assert_eq!(*count, 2, "count should still reflect actual stories");
        assert!(!expanded, "section should be marked as collapsed");
    }
}

// ─── Test 9: Multiple stories across states ─────────────────────────

#[test]
fn stories_grouped_correctly_across_multiple_states() {
    let (_dir, root) = init_project("GR");

    // `in-progress` is a default state.
    let id1 = create_story(&root, "Todo story");
    let id2 = create_story(&root, "In progress story");
    let id3 = create_story(&root, "Another todo");

    // Move id2 to in-progress
    set_state(&root, &id2, "in-progress");

    let store = load(&root).unwrap();
    let state = AppState::new(store);
    let board = Board::new();
    let rows = board.build_visible_rows(&state);

    // Verify the structure: todo header, 2 stories, in-progress header, 1 story
    let mut expected_sequence = vec![];
    for row in &rows {
        match row {
            RowItem::SectionHeader { slug, count, .. } => {
                expected_sequence.push(format!("section:{slug}:{count}"));
            }
            RowItem::StoryRow { id } => {
                expected_sequence.push(format!("story:{id}"));
            }
        }
    }

    assert!(expected_sequence.contains(&"section:todo:2".to_string()));
    assert!(expected_sequence.contains(&"section:in-progress:1".to_string()));
    assert!(expected_sequence.contains(&format!("story:{id1}")));
    assert!(expected_sequence.contains(&format!("story:{id2}")));
    assert!(expected_sequence.contains(&format!("story:{id3}")));
}

// ─── Test 10: View switching preserves state ────────────────────────

#[test]
fn view_switching_preserves_board_state() {
    let (_dir, root) = init_project("VS");
    create_story(&root, "Test story");

    let store = load(&root).unwrap();
    let mut state = AppState::new(store);

    // Set up some board state
    state.view = View::Board;
    state.focus.base = FocusTarget::Board;
    state.filters.push(FilterSpec {
        text: Some("test".to_string()),
        ..Default::default()
    });

    // Switch to dashboard
    state.view = View::Dashboard;
    state.focus.base = FocusTarget::Dashboard;

    // Switch back to board
    state.view = View::Board;
    state.focus.base = FocusTarget::Board;

    // Filters should be preserved
    assert_eq!(state.filters.len(), 1);
    assert_eq!(state.filters[0].text.as_deref(), Some("test"));
}

// ─── Test 11: Data refresh after external write ─────────────────────

#[test]
fn data_refresh_picks_up_external_changes() {
    let (_dir, root) = init_project("RX");
    let id = create_story(&root, "Original");

    let store = load(&root).unwrap();
    assert_eq!(store.find_story(&id).unwrap().priority, Priority::None);

    // External write (simulating CLI changing priority)
    set_priority(&root, &id, "critical");

    // Refresh
    let store = load(&root).unwrap();
    assert_eq!(
        store.find_story(&id).unwrap().priority,
        Priority::Critical,
        "refresh should pick up external priority change"
    );
}

// ════════════��═══════════════════���══════════════════════════════════════
// Edge Case Tests (Task 6.2)
// ═════════════════════════��═════════════════════════════════════════════

// ─── Edge 1: Story deleted while detail modal is open ───────────────

#[test]
fn story_deleted_externally_closes_modal_with_notification() {
    let (_dir, root) = init_project("ED");
    let id = create_story(&root, "Ephemeral");

    let store = load(&root).unwrap();
    let mut state = AppState::new(store);

    // Open detail modal for the story
    state.focus.push_modal(Modal::StoryDetail {
        story_id: id.clone(),
    });

    // Externally delete the story file (simulating a race condition)
    let story_file = root.join(format!(".storyhook/open/stories/{id}.jsonl"));
    std::fs::remove_file(&story_file).unwrap();

    // Refresh data
    let new_data = load(&root).unwrap();
    state.data = new_data;

    // Stale modal protection logic (mirrors app.rs dispatch for RefreshData)
    if let Some(Modal::StoryDetail { story_id }) = state.focus.top_modal()
        && state.data.find_story(story_id).is_none()
    {
        let stale_id = story_id.clone();
        state.focus.pop_modal();
        state.notification = Some((format!("Story {stale_id} no longer open"), Instant::now()));
    }

    assert!(!state.focus.has_modal());
    assert!(state.notification.is_some());
}

// ─── Edge 2: Empty states.toml (no valid states) ────────────────────

#[test]
fn empty_states_file_produces_validation_error() {
    let (_dir, root) = init_project("ES");

    // Overwrite states.toml with empty states
    std::fs::write(root.join(".storyhook/states.toml"), "states = []\n").unwrap();

    // DataStore::load should return an error, not panic
    let result = load(&root);
    assert!(
        result.is_err(),
        "empty states should produce a validation error"
    );
    let err_msg = format!("{}", result.err().unwrap());
    assert!(
        err_msg.contains("OPEN") || err_msg.contains("state"),
        "error should mention state validation: {err_msg}"
    );
}

// ─── Edge 3: Very long title truncation ────────────────��────────────

#[test]
fn very_long_title_does_not_panic_in_board() {
    let (_dir, root) = init_project("LT");
    let long_title = "A".repeat(300);
    create_story(&root, &long_title);

    let store = load(&root).unwrap();
    let story = store.find_story("LT-1").unwrap();
    assert_eq!(story.title.len(), 300);

    // Build board rows -- this should not panic regardless of title length
    let state = AppState::new(store);
    let board = Board::new();
    let rows = board.build_visible_rows(&state);
    assert!(
        rows.iter()
            .any(|r| matches!(r, RowItem::StoryRow { id } if id == "LT-1")),
        "long-titled story should appear in board rows"
    );
}

// ─── Edge 4: Story with 20+ labels ─────────────��───────────────────

#[test]
fn many_labels_do_not_panic() {
    let (_dir, root) = init_project("ML");
    let id = create_story(&root, "Many labels");

    let labels: Vec<String> = (0..25).map(|i| format!("label-{i}")).collect();
    run(
        &root,
        Invocation::SetLabels {
            id: id.clone(),
            add: labels,
            remove: Vec::new(),
        },
    )
    .unwrap();

    let store = load(&root).unwrap();
    let story = store.find_story(&id).unwrap();
    assert_eq!(story.labels.len(), 25);

    // Build board rows -- should not panic
    let state = AppState::new(store);
    let board = Board::new();
    let rows = board.build_visible_rows(&state);
    assert!(
        rows.iter().any(|r| matches!(r, RowItem::StoryRow { .. })),
        "story with many labels should appear in board rows"
    );
}

// ─── Edge 5: Unicode in titles, labels, and comments ────────────────

#[test]
fn unicode_content_handled_gracefully() {
    let (_dir, root) = init_project("UC");
    let id = create_story(&root, "Fix emoji rendering \u{1F680}\u{1F30D}");

    set_labels(
        &root,
        &id,
        &[
            "\u{2705} done",
            "\u{00E9}t\u{00E9}", // "ete" with accents
            "\u{4F60}\u{597D}",  // Chinese: "hello"
        ],
    );
    run(
        &root,
        Invocation::Comment {
            id: id.clone(),
            text: "Added CJK characters: \u{65E5}\u{672C}\u{8A9E}".to_string(),
        },
    )
    .unwrap();

    let store = load(&root).unwrap();
    let story = store.find_story(&id).unwrap();
    assert!(story.title.contains('\u{1F680}'));
    assert_eq!(story.labels.len(), 3);
    assert_eq!(story.comments.len(), 1);

    // Build board rows -- should not panic
    let state = AppState::new(store);
    let board = Board::new();
    let rows = board.build_visible_rows(&state);
    assert!(
        rows.iter().any(|r| matches!(r, RowItem::StoryRow { .. })),
        "story with unicode should appear in board rows"
    );
}

// ─── Edge 6: Concurrent write tolerance (incomplete trailing line) ──

#[test]
fn incomplete_trailing_json_line_tolerated() {
    let (_dir, root) = init_project("CW");
    let id = create_story(&root, "Concurrent write test");

    // Append a partial JSON line (simulating a concurrent write in progress)
    let story_path = root.join(format!(".storyhook/open/stories/{id}.jsonl"));
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&story_path)
            .unwrap();
        write!(file, "\n{{\"kind\":\"StoryCommentAdd").unwrap();
    }

    // DataStore::load should succeed, skipping the incomplete line
    let store = load(&root).unwrap();
    assert_eq!(store.story_count(), 1);
    assert_eq!(
        store.find_story(&id).unwrap().title,
        "Concurrent write test"
    );
}

// ─── Edge 7: Refresh after state removal from states.toml ───────────

#[test]
fn state_removal_during_runtime_returns_error() {
    let (_dir, root) = init_project("SR");

    // Add "review" state
    run(
        &root,
        Invocation::State {
            action: StateAction::Add {
                slug: "review".to_string(),
                superstate: "OPEN".to_string(),
                role: None,
                description: None,
            },
        },
    )
    .unwrap();

    let id = create_story(&root, "Will lose its state");

    // Move to review
    set_state(&root, &id, "review");

    // Verify it loads fine with "review" still defined
    let store = load(&root).unwrap();
    assert_eq!(store.find_story(&id).unwrap().state, "review");

    // Now remove the "review" state out from under the story. `story state
    // remove` refuses while a story occupies it — which is the point: this
    // case fabricates a project the API will not produce, so it writes the
    // catalog directly.
    let states = vec![
        StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
        StateDef {
            slug: "done".to_string(),
            super_state: SuperState::Closed,
            role: None,
            description: None,
        },
    ];
    let states_toml = toml::to_string(&StatesFileHelper { states: &states }).unwrap();
    std::fs::write(root.join(".storyhook/states.toml"), states_toml).unwrap();

    // DataStore::load returns an error because fold_story validates that
    // the story's state exists in the state map. This error is caught by
    // the TUI's RefreshData handler and shown as a notification.
    let result = load(&root);
    assert!(result.is_err(), "should error on undefined state reference");
    let err_msg = format!("{}", result.err().unwrap());
    assert!(
        err_msg.contains("undefined state"),
        "error should mention undefined state: {err_msg}"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Performance Check (Task 6.3)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn datastore_load_100_stories_under_500ms() {
    let (_dir, root) = init_project("PF");

    // Create 100 stories with varying attributes
    for i in 1..=100 {
        create_story(&root, &format!("Performance test story {i}"));
    }

    // Add events to some stories (priorities, labels, comments)
    for i in 1..=50 {
        let id = format!("PF-{i}");
        set_priority(
            &root,
            &id,
            match i % 4 {
                0 => "critical",
                1 => "high",
                2 => "medium",
                _ => "low",
            },
        );
        if i % 3 == 0 {
            set_labels(&root, &id, &["perf", "test"]);
        }
        if i % 5 == 0 {
            run(
                &root,
                Invocation::Comment {
                    id: id.clone(),
                    text: format!("Comment on story {i}"),
                },
            )
            .unwrap();
        }
    }

    // Measure load time
    let start = Instant::now();
    let store = load(&root).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(store.story_count(), 100);
    assert!(
        elapsed.as_millis() < 500,
        "DataStore::load for 100 stories took {}ms, should be under 500ms",
        elapsed.as_millis()
    );

    // Also verify board building is fast
    let state = AppState::new(store);
    let board = Board::new();
    let start = Instant::now();
    let rows = board.build_visible_rows(&state);
    let elapsed = start.elapsed();

    assert!(rows.len() > 100); // 100 stories + section headers
    assert!(
        elapsed.as_millis() < 50,
        "build_visible_rows for 100 stories took {}ms, should be under 50ms",
        elapsed.as_millis()
    );
}

// ─── Statuses editor (SH-41) ────────────────────────────────────────
//
// The component's key handling is unit-tested in
// `tui::components::states_editor`; these exercise the other half — that
// the actions it emits, applied through the same storage operations the
// dispatch loop calls, actually change the project and are reflected by a
// reload.

/// The board renders one column per OPEN state, in configured order — so a
/// reorder from the editor is visible work, not bookkeeping.
#[test]
fn reordering_statuses_reorders_the_board_columns() {
    let (_dir, root) = init_project("TP");
    let before: Vec<String> = load(&root)
        .unwrap()
        .states
        .iter()
        .map(|s| s.slug.clone())
        .collect();
    assert_eq!(before, vec!["todo", "in-progress", "done"]);

    run(
        &root,
        Invocation::State {
            action: StateAction::Reorder {
                order: vec![
                    "in-progress".to_string(),
                    "todo".to_string(),
                    "done".to_string(),
                ],
            },
        },
    )
    .unwrap();

    let store = load(&root).unwrap();
    let columns: Vec<&str> = store
        .stories_by_state()
        .iter()
        .map(|(state, _)| state.slug.as_str())
        .collect();
    assert_eq!(
        columns,
        vec!["in-progress", "todo"],
        "OPEN states, in order"
    );
}

#[test]
fn adding_a_status_makes_it_available_to_the_board() {
    let (_dir, root) = init_project("TP");
    run(
        &root,
        Invocation::State {
            action: StateAction::Add {
                slug: "review".to_string(),
                superstate: "OPEN".to_string(),
                role: None,
                description: Some("Waiting on a reviewer".to_string()),
            },
        },
    )
    .unwrap();

    let store = load(&root).unwrap();
    let review = store
        .states
        .iter()
        .find(|s| s.slug == "review")
        .expect("the new status should load");
    assert_eq!(review.description.as_deref(), Some("Waiting on a reviewer"));
    assert!(
        store
            .stories_by_state()
            .iter()
            .any(|(state, _)| state.slug == "review")
    );
}

/// The editor's migration path, end to end: the story moves and the status
/// is gone afterwards.
#[test]
fn removing_an_occupied_status_migrates_its_stories() {
    let (_dir, root) = init_project("TP");
    let id = create_story(&root, "Needs a home");

    run(
        &root,
        Invocation::State {
            action: StateAction::Remove {
                slug: "todo".to_string(),
                move_stories_to: Some("in-progress".to_string()),
            },
        },
    )
    .unwrap();

    let store = load(&root).unwrap();
    assert!(!store.states.iter().any(|s| s.slug == "todo"));
    assert_eq!(store.find_story(&id).unwrap().state, "in-progress");
}

/// Without a destination the storage layer refuses, and the TUI turns that
/// error into a notification rather than losing the stories.
#[test]
fn removing_an_occupied_status_without_a_destination_is_refused() {
    let (_dir, root) = init_project("TP");
    create_story(&root, "Still here");

    let error = run(
        &root,
        Invocation::State {
            action: StateAction::Remove {
                slug: "todo".to_string(),
                move_stories_to: None,
            },
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("1 open story"));

    let store = load(&root).unwrap();
    assert!(store.states.iter().any(|s| s.slug == "todo"));
}

// ─── Helper struct for TOML serialization of states ─────────────────

#[derive(serde::Serialize)]
struct StatesFileHelper<'a> {
    states: &'a [StateDef],
}
