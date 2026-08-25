//! Integration tests for the TUI data path.
//!
//! These tests exercise DataStore + AppState + Board + Action dispatch
//! WITHOUT launching a terminal, verifying the full data flow from creation
//! through filtering and display.
//!
//! **Reconstructed for the Invoker seam**, then **moved onto the store**, as
//! `tui_undo.rs` already was. Fixtures are built out of the same `Invocation`s
//! the TUI itself issues, and `DataStore::load` takes an `Invoker` — a
//! `StoreInvoker`, which is what the TUI runs against.
//!
//! Three fixtures used to fabricate their subject by writing into
//! `.storyhook/` because no API could produce it. Two of them still fabricate
//! it, through the store's own write API rather than a file: a state set with
//! nothing OPEN in it, and a state set that no longer defines a state a story
//! occupies. Both are states the *service* layer refuses and the *store* will
//! hold, which is exactly what makes them worth loading.
//!
//! The third is gone: `incomplete_trailing_json_line_tolerated` appended half a
//! JSON object to a story's `.jsonl` and asserted the reader skipped it. An
//! event is a row now, written inside a transaction, so a half-written one is
//! not a state the storage layer can be in. The tolerance it tested was a
//! property of a file format, and the file format is what the rearchitecture
//! removed.

use std::time::Instant;

use storyhook::cli::{
    Attach, Invocation, NewProjectRequest, NewProjectSpec, ProjectAction, StateAction,
};
use storyhook::domain::{Priority, StateDef, SuperState};
use storyhook::env::Environment;
use storyhook::error::AppError;
use storyhook::invoke::{InvokeRequest, Invoker, StoreInvoker};
use storyhook::output::Response;
use storyhook::store::{ProjectId, SqliteStore, Store as _, diff_read_model};
use storyhook::tui::action::{FilterSpec, View};
use storyhook::tui::components::board::{Board, RowItem};
use storyhook::tui::components::dashboard::ready_stories;
use storyhook::tui::data::DataStore;
use storyhook::tui::focus::{FocusStack, FocusTarget, Modal};
use storyhook::tui::state::AppState;

/// A store and a checkout with a project initialized in it.
///
/// The store's directory is a fixture of its own rather than the machine's:
/// these are in-process tests, and an in-process test cannot redirect
/// `STORYHOOK_DATA_DIR` for itself.
struct Fixture {
    store: SqliteStore,
    root: std::path::PathBuf,
    env: Environment,
    _data: tempfile::TempDir,
    _repo: tempfile::TempDir,
}

impl Fixture {
    fn invoker(&self) -> StoreInvoker<'_, SqliteStore> {
        StoreInvoker::new(&self.store, &self.root, self.env.clone())
    }

    /// The project this checkout belongs to.
    fn project(&self) -> ProjectId {
        storyhook_test_support::project_id_at(&self.store, &self.root)
            .expect("the fixture's checkout must name a project")
    }
}

/// Helper: run one invocation through the seam, hooks suppressed as the TUI
/// does.
fn run(fixture: &Fixture, invocation: Invocation) -> Result<Response, AppError> {
    fixture
        .invoker()
        .invoke(InvokeRequest::new(invocation).no_hooks(true))
}

/// Helper: the project as the TUI sees it — one `ProjectSnapshot`.
fn load(fixture: &Fixture) -> Result<DataStore, AppError> {
    DataStore::load(&fixture.invoker())
}

/// Helper: a state beyond the required floor, for the tests whose subject is
/// what removing a state does to its occupants.
///
/// The four states every project must have cannot be removed at any occupancy
/// (SH-125), so those tests need a fifth.
fn add_state(fixture: &Fixture, slug: &str) {
    run(
        fixture,
        Invocation::State {
            action: StateAction::Add {
                slug: slug.to_string(),
                superstate: "OPEN".to_string(),
                role: None,
                description: None,
            },
        },
    )
    .unwrap();
}

/// Helper: a store and a checkout with a project in it.
fn init_project(prefix: &str) -> Fixture {
    let data = storyhook_test_support::scratch_dir();
    let repo = storyhook_test_support::scratch_dir();
    let env = Environment::at(data.path());
    let store = SqliteStore::open(env.store_path()).unwrap();
    store.migrate().unwrap();
    let fixture = Fixture {
        store,
        root: repo.path().to_path_buf(),
        env,
        _data: data,
        _repo: repo,
    };
    run(
        &fixture,
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                attach: Attach::Cwd,
                prefix: prefix.to_string(),
                name: None,
                no_agents_md: true,
            })),
        },
    )
    .unwrap();
    fixture
}

/// Replaces the project's state set, bypassing the validation the service
/// layer applies on the way in.
///
/// Two tests need a catalog `story state …` refuses to create: one with no
/// OPEN state at all, and one that has dropped a state a story still occupies.
/// They used to write `states.toml`; the store's write API is the equivalent
/// back door, and it is a back door on purpose — what is under test is whether
/// the *reader* survives such a project, which means something has to be able
/// to make one.
fn force_states(fixture: &Fixture, states: &[StateDef]) {
    // Since SH-130 the store's own write API cannot do this either: `stories`
    // carries a composite foreign key into `project_states`, so dropping a
    // state a story still occupies fails at COMMIT. The fabrication moves down
    // one more layer, to the corruption API whose whole job is producing shapes
    // the schema refuses.
    storyhook::store::test_support::replace_states(&fixture.store, fixture.project(), states)
        .expect("replacing the state set");
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

/// Helper: add a member so `story assign` has someone to resolve to.
fn add_member(fixture: &Fixture, handle: &str) {
    run(
        fixture,
        Invocation::MemberAdd {
            input: storyhook::cli::MemberInput::Github(handle.to_string()),
        },
    )
    .unwrap();
}

/// Helper: move a story into a state.
fn set_state(fixture: &Fixture, id: &str, state: &str) {
    run(
        fixture,
        Invocation::SetState {
            id: id.to_string(),
            state: state.to_string(),
            comment: None,
            if_state: None,
            awaiting: None,
        },
    )
    .unwrap();
}

/// Helper: set a story's labels, which for a story that has none is an add.
fn set_labels(fixture: &Fixture, id: &str, labels: &[&str]) {
    run(
        fixture,
        Invocation::SetLabels {
            id: id.to_string(),
            add: labels.iter().map(|l| (*l).to_string()).collect(),
            remove: Vec::new(),
        },
    )
    .unwrap();
}

/// Helper: set a story's priority.
fn set_priority(fixture: &Fixture, id: &str, priority: &str) {
    run(
        fixture,
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
    let fixture = init_project("TP");
    let store = load(&fixture).unwrap();

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
    let fixture = init_project("CE");

    // Create a story
    let id = create_story(&fixture, "Initial title");
    assert_eq!(id, "CE-1");

    // Verify it appears in DataStore
    let store = load(&fixture).unwrap();
    assert_eq!(store.story_count(), 1);
    let story = store.find_story("CE-1").unwrap();
    assert_eq!(story.title, "Initial title");
    assert_eq!(story.priority, Priority::Low);
    assert!(story.assignee.is_none());
    assert!(story.comments.is_empty());

    // Set priority
    set_priority(&fixture, "CE-1", "high");
    let store = load(&fixture).unwrap();
    assert_eq!(store.find_story("CE-1").unwrap().priority, Priority::High);

    // Assign
    add_member(&fixture, "mikey");
    run(
        &fixture,
        Invocation::Assign {
            id: "CE-1".to_string(),
            member: "mikey".to_string(),
        },
    )
    .unwrap();
    let store = load(&fixture).unwrap();
    assert_eq!(
        store.find_story("CE-1").unwrap().assignee.as_deref(),
        Some("mikey")
    );

    // Add comment
    run(
        &fixture,
        Invocation::Comment {
            id: "CE-1".to_string(),
            text: "Work in progress".to_string(),
        },
    )
    .unwrap();
    let store = load(&fixture).unwrap();
    assert_eq!(store.find_story("CE-1").unwrap().comments.len(), 1);
    assert_eq!(
        store.find_story("CE-1").unwrap().comments[0].text,
        "Work in progress"
    );
}

// ─── Test 3: Move story through states ──────────────────────────────

#[test]
fn move_story_through_states() {
    let fixture = init_project("MV");

    // `in-progress` is a default state, so the board already has a second
    // OPEN column to move into.
    let id = create_story(&fixture, "Move me");

    // Story starts in "todo" (default open state)
    let store = load(&fixture).unwrap();
    assert_eq!(store.find_story(&id).unwrap().state, "todo");

    // Move to in-progress
    set_state(&fixture, &id, "in-progress");
    let store = load(&fixture).unwrap();
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
    let fixture = init_project("CL");
    let id = create_story(&fixture, "Close me");

    // Story exists in open snapshots
    let store = load(&fixture).unwrap();
    assert_eq!(store.story_count(), 1);

    // Move to "done" (closed state) - archive it
    set_state(&fixture, &id, "done");

    // Story should be gone from open snapshots
    let store = load(&fixture).unwrap();
    assert_eq!(store.story_count(), 0);
    assert!(store.find_story(&id).is_none());
}

// ─── Test 5: Filter application ─────────────────────────────────────

#[test]
fn filter_narrows_visible_stories() {
    let fixture = init_project("FL");

    let id1 = create_story(&fixture, "Fix login bug");
    let id2 = create_story(&fixture, "Add search feature");
    let id3 = create_story(&fixture, "Refactor database");

    // Set different priorities and labels
    add_member(&fixture, "mikey");
    set_priority(&fixture, &id1, "high");
    set_labels(&fixture, &id1, &["bug"]);
    set_priority(&fixture, &id2, "medium");
    set_labels(&fixture, &id3, &["tech-debt"]);
    run(
        &fixture,
        Invocation::Assign {
            id: id3.clone(),
            member: "mikey".to_string(),
        },
    )
    .unwrap();

    let store = load(&fixture).unwrap();
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
    let store = load(&fixture).unwrap();
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
    let store = load(&fixture).unwrap();
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
    let store = load(&fixture).unwrap();
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
    let fixture = init_project("SM");
    let id = create_story(&fixture, "Will be archived");

    let store = load(&fixture).unwrap();
    let mut state = AppState::new(store);

    // Simulate opening the detail modal
    state.focus.push_modal(Modal::StoryDetail {
        story_id: id.clone(),
    });
    assert!(state.focus.has_modal());

    // Now archive the story externally (simulating CLI action)
    set_state(&fixture, &id, "done");

    // Refresh data (simulates what RefreshData action does)
    let new_data = load(&fixture).unwrap();
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
    let fixture = init_project("CS");
    create_story(&fixture, "Story A");
    create_story(&fixture, "Story B");

    let store = load(&fixture).unwrap();
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
    let fixture = init_project("GR");

    // `in-progress` is a default state.
    let id1 = create_story(&fixture, "Todo story");
    let id2 = create_story(&fixture, "In progress story");
    let id3 = create_story(&fixture, "Another todo");

    // Move id2 to in-progress
    set_state(&fixture, &id2, "in-progress");

    let store = load(&fixture).unwrap();
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
    let fixture = init_project("VS");
    create_story(&fixture, "Test story");

    let store = load(&fixture).unwrap();
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
    let fixture = init_project("RX");
    let id = create_story(&fixture, "Original");

    let store = load(&fixture).unwrap();
    assert_eq!(store.find_story(&id).unwrap().priority, Priority::Low);

    // External write (simulating CLI changing priority)
    set_priority(&fixture, &id, "critical");

    // Refresh
    let store = load(&fixture).unwrap();
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
    let fixture = init_project("ED");
    let id = create_story(&fixture, "Ephemeral");

    let store = load(&fixture).unwrap();
    let mut state = AppState::new(store);

    // Open detail modal for the story
    state.focus.push_modal(Modal::StoryDetail {
        story_id: id.clone(),
    });

    // The story goes away underneath the open modal. It used to go away by
    // `rm`-ing its `.jsonl`; a story leaves the project through `story delete`
    // now, and what is under test is the modal's reaction to a story that has
    // stopped being there, not how it stopped.
    run(
        &fixture,
        Invocation::Delete {
            id: id.clone(),
            reason: "raced".to_string(),
        },
    )
    .unwrap();

    // Refresh data
    let new_data = load(&fixture).unwrap();
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

// ─── Edge 2: a catalog that no longer covers the project ────────────

/// A project whose state set has been emptied out from under it still opens,
/// and the damage is reported by the integrity check rather than by refusing
/// to read.
///
/// **This is a behaviour change, and it is the intended one.** The legacy
/// reader re-folded every story against `states.toml` on every read, so an
/// unusable catalog made the project *unreadable* — the exact hazard
/// `domain::validate_state_defs` warns about in its own doc comment ("a rule
/// added here can make an existing project unreadable rather than merely
/// uneditable"). The store splits the two questions: the read model is already
/// folded, so a reader is not hostage to the catalog, and
/// `diff_read_model` — `story doctor`'s integrity check — is what says the
/// events and the catalog disagree.
#[test]
fn an_emptied_state_set_still_loads_and_is_reported_by_the_integrity_check() {
    let fixture = init_project("ES");
    let id = create_story(&fixture, "Occupying todo");

    // `story state remove` refuses to leave a project with no states, so this
    // goes in through the store's own write API.
    force_states(&fixture, &[]);

    let data = load(&fixture).expect("the TUI must still open the project");
    assert_eq!(data.story_count(), 1);
    assert_eq!(data.find_story(&id).unwrap().state, "todo");

    let diff = diff_read_model(&fixture.store, fixture.project()).expect("the integrity check");
    assert!(
        diff.has_integrity_issues(),
        "an emptied catalog must be reported, not ignored: {}",
        diff.describe()
    );
    assert!(
        diff.describe().contains("undefined state"),
        "and must name what is wrong: {}",
        diff.describe()
    );
}

// ─── Edge 3: Very long title truncation ────────────────��────────────

#[test]
fn very_long_title_does_not_panic_in_board() {
    let fixture = init_project("LT");
    let long_title = "A".repeat(300);
    create_story(&fixture, &long_title);

    let store = load(&fixture).unwrap();
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
    let fixture = init_project("ML");
    let id = create_story(&fixture, "Many labels");

    let labels: Vec<String> = (0..25).map(|i| format!("label-{i}")).collect();
    run(
        &fixture,
        Invocation::SetLabels {
            id: id.clone(),
            add: labels,
            remove: Vec::new(),
        },
    )
    .unwrap();

    let store = load(&fixture).unwrap();
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
    let fixture = init_project("UC");
    let id = create_story(&fixture, "Fix emoji rendering \u{1F680}\u{1F30D}");

    set_labels(
        &fixture,
        &id,
        &[
            "\u{2705} done",
            "\u{00E9}t\u{00E9}", // "ete" with accents
            "\u{4F60}\u{597D}",  // Chinese: "hello"
        ],
    );
    run(
        &fixture,
        Invocation::Comment {
            id: id.clone(),
            text: "Added CJK characters: \u{65E5}\u{672C}\u{8A9E}".to_string(),
        },
    )
    .unwrap();

    let store = load(&fixture).unwrap();
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

// ─── Edge 6: deleted with its subject ───────────────────────────────
//
// `incomplete_trailing_json_line_tolerated` appended half a JSON object to a
// story's `.jsonl` and asserted the reader skipped it, because an append-mode
// write to a text file can be interrupted between the bytes. An event is a row
// inside a transaction now: a half-written one is not a state the storage layer
// can be in, and the tolerance the test pinned was a property of the file
// format rather than of the TUI. Crash-mid-write is still tested — against the
// store, where it means something, by `tests/store_*.rs`'s fault-injection
// points.

// ─── Edge 7: Refresh after a state is removed from the catalog ──────

#[test]
fn state_removal_during_runtime_is_survivable_and_reported() {
    let fixture = init_project("SR");

    // Add "review" state
    run(
        &fixture,
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

    let id = create_story(&fixture, "Will lose its state");

    // Move to review
    set_state(&fixture, &id, "review");

    // Verify it loads fine with "review" still defined
    let store = load(&fixture).unwrap();
    assert_eq!(store.find_story(&id).unwrap().state, "review");

    // Now remove the "review" state out from under the story. `story state
    // remove` refuses while a story occupies it — which is the point: this
    // case fabricates a project the API will not produce, so it goes in
    // through the store.
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
    force_states(&fixture, &states);

    // The story keeps the state it is in — the read model holds a folded
    // snapshot, not a re-derivation — so the TUI refreshes rather than failing.
    // See `an_emptied_state_set_still_loads_and_is_reported_by_the_integrity_check`
    // for why that is the intended change.
    let refreshed = load(&fixture).expect("a refresh must survive a catalog edit");
    assert_eq!(refreshed.find_story(&id).unwrap().state, "review");

    let diff = diff_read_model(&fixture.store, fixture.project()).expect("the integrity check");
    assert!(
        diff.describe().contains("undefined state"),
        "removing a state out from under a story must be reported: {}",
        diff.describe()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Load shape at 100 stories
// ═══════════════════════════════════════════════════════════════════════

/// An [`Invoker`] that counts what passes through it and forwards the rest.
///
/// The seam is the only place a round trip to the store can be observed from
/// outside, which is what makes "one invocation" assertable rather than merely
/// documented.
struct CountingInvoker<'a> {
    inner: StoreInvoker<'a, SqliteStore>,
    calls: std::cell::Cell<usize>,
}

impl Invoker for CountingInvoker<'_> {
    fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError> {
        self.calls.set(self.calls.get() + 1);
        self.inner.invoke(request)
    }
}

/// `DataStore::load` reaches the store **once**, however many stories it holds.
///
/// **This replaced a stopwatch, and the replacement is the point (SH-140).**
/// The assertion here used to be `elapsed < 500ms` over the same fixture. That
/// number was 500x the measured 1.0ms, so the regression it existed to catch —
/// a query per story, or per event — could grow the work a hundredfold and
/// still pass. It could only fail for a reason that has nothing to do with the
/// code: this fixture is a real SQLite database, and a stalled filesystem
/// blows any wall-clock figure while the load itself is unchanged.
///
/// So the promise is asserted where it is actually made. `DataStore`'s own doc
/// says it loads "in one invocation", and that is a property of the seam rather
/// than of the machine: it is exact, it is the same on every host, and an N+1
/// fails it at the first extra round trip instead of the hundredth.
#[test]
fn loading_a_hundred_stories_is_one_invocation() {
    let fixture = init_project("PF");

    // Create 100 stories with varying attributes
    for i in 1..=100 {
        create_story(&fixture, &format!("Performance test story {i}"));
    }

    // Add events to some stories (priorities, labels, comments)
    for i in 1..=50 {
        let id = format!("PF-{i}");
        set_priority(
            &fixture,
            &id,
            match i % 4 {
                0 => "critical",
                1 => "high",
                2 => "medium",
                _ => "low",
            },
        );
        if i % 3 == 0 {
            set_labels(&fixture, &id, &["perf", "test"]);
        }
        if i % 5 == 0 {
            run(
                &fixture,
                Invocation::Comment {
                    id: id.clone(),
                    text: format!("Comment on story {i}"),
                },
            )
            .unwrap();
        }
    }

    let counting = CountingInvoker {
        inner: fixture.invoker(),
        calls: std::cell::Cell::new(0),
    };
    let store = DataStore::load(&counting).expect("loading the project");

    assert_eq!(store.story_count(), 100);
    assert_eq!(
        counting.calls.get(),
        1,
        "loading 100 stories took {} round trips to the store; `DataStore` \
         promises one, and anything per-story is the N+1 this guards",
        counting.calls.get()
    );

    // Row building is a fold over what that one load returned. It used to carry
    // a 50ms budget, deleted with the one above: `AppState` holds a `DataStore`,
    // which is a plain in-memory snapshot with no connection, no `Invoker` and
    // no path, so row building *cannot* reach the store — the regression a
    // timing bound would be watching for is one the types already refuse. What
    // is worth asserting is that every story survives the fold exactly once.
    let state = AppState::new(store);
    let board = Board::new();
    let rows = board.build_visible_rows(&state);

    let story_rows = rows
        .iter()
        .filter(|row| matches!(row, RowItem::StoryRow { .. }))
        .count();
    let headers = rows
        .iter()
        .filter(|row| matches!(row, RowItem::SectionHeader { .. }))
        .count();
    assert_eq!(story_rows, 100, "every story must appear exactly once");
    assert!(headers > 0, "the board must draw its section headers");
    assert_eq!(
        rows.len(),
        story_rows + headers,
        "the board must draw nothing it cannot account for"
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
    let fixture = init_project("TP");
    let before: Vec<String> = load(&fixture)
        .unwrap()
        .states
        .iter()
        .map(|s| s.slug.clone())
        .collect();
    assert_eq!(before, vec!["todo", "in-progress", "blocked", "done"]);

    run(
        &fixture,
        Invocation::State {
            action: StateAction::Reorder {
                order: vec![
                    "in-progress".to_string(),
                    "todo".to_string(),
                    "blocked".to_string(),
                    "done".to_string(),
                ],
            },
        },
    )
    .unwrap();

    let store = load(&fixture).unwrap();
    let columns: Vec<&str> = store
        .stories_by_state()
        .iter()
        .map(|(state, _)| state.slug.as_str())
        .collect();
    assert_eq!(
        columns,
        vec!["in-progress", "todo", "blocked"],
        "OPEN states, in order"
    );
}

#[test]
fn adding_a_status_makes_it_available_to_the_board() {
    let fixture = init_project("TP");
    run(
        &fixture,
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

    let store = load(&fixture).unwrap();
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
    let fixture = init_project("TP");
    add_state(&fixture, "in-review");
    let id = create_story(&fixture, "Needs a home");
    run(
        &fixture,
        Invocation::SetState {
            id: id.clone(),
            state: "in-review".to_string(),
            comment: None,
            if_state: None,
            awaiting: None,
        },
    )
    .unwrap();

    run(
        &fixture,
        Invocation::State {
            action: StateAction::Remove {
                slug: "in-review".to_string(),
                move_stories_to: Some("in-progress".to_string()),
            },
        },
    )
    .unwrap();

    let store = load(&fixture).unwrap();
    assert!(!store.states.iter().any(|s| s.slug == "in-review"));
    assert_eq!(store.find_story(&id).unwrap().state, "in-progress");
}

/// Without a destination the storage layer refuses, and the TUI turns that
/// error into a notification rather than losing the stories.
#[test]
fn removing_an_occupied_status_without_a_destination_is_refused() {
    let fixture = init_project("TP");
    add_state(&fixture, "in-review");
    let id = create_story(&fixture, "Still here");
    run(
        &fixture,
        Invocation::SetState {
            id,
            state: "in-review".to_string(),
            comment: None,
            if_state: None,
            awaiting: None,
        },
    )
    .unwrap();

    let error = run(
        &fixture,
        Invocation::State {
            action: StateAction::Remove {
                slug: "in-review".to_string(),
                move_stories_to: None,
            },
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("1 open story"));

    let store = load(&fixture).unwrap();
    assert!(store.states.iter().any(|s| s.slug == "in-review"));
}

/// SH-240/SH-450, end to end: the dashboard's "Ready Stories" panel and
/// `story next` agree on the immediately actionable head of the same real
/// project. Multi-result `next` then continues through work its earlier
/// results would unblock, while the panel remains a snapshot of readiness now.
///
/// The unit tests around `ready_stories` build a `DataStore` by hand, so
/// they cannot see whether the *snapshot* carries what the answer needs —
/// the project's real state catalog (which slug means "claimed"), and the
/// drafts that live beside `stories`. This one goes through the seam the TUI
/// runs on, so it does.
///
/// The fixture deliberately holds no epic: `story next` additionally
/// excludes a story with children, which is a `next` rule rather than a
/// readiness one, so an epic would make the two disagree for a reason that
/// is not this test's subject.
#[test]
fn the_ready_panel_and_story_next_share_the_actionable_head() {
    let fixture = init_project("SH");

    let free = create_story(&fixture, "Free");
    let claimed = create_story(&fixture, "Claimed");
    set_state(&fixture, &claimed, "in-progress");
    let parked = create_story(&fixture, "Parked");
    set_state(&fixture, &parked, "blocked");
    let blocker = create_story(&fixture, "Blocker");
    let dependent = create_story(&fixture, "Waits on the blocker");
    relate(&fixture, &dependent, "blocked-by", &blocker);
    let draft = create_draft(&fixture, "Still being specified");
    let waiting_on_a_draft = create_story(&fixture, "Waits on the draft");
    relate(&fixture, &waiting_on_a_draft, "blocked-by", &draft);

    let data = load(&fixture).unwrap();
    assert_eq!(
        data.drafts
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        [draft.as_str()],
        "the snapshot's drafts have to reach the DataStore, or a draft \
         blocker is invisible to the readiness walk"
    );

    let panel: Vec<&str> = ready_stories(&data).iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        panel,
        [free.as_str(), blocker.as_str()],
        "claimed, parked, blocked-by and draft-blocked work is not offered"
    );

    // The board's `ready` chip is the same claim through a different
    // surface, so it answers the same set.
    let chip = data.filter(&[FilterSpec {
        ready: true,
        ..Default::default()
    }]);
    assert_eq!(
        chip.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        panel,
        "the board's `ready` chip and the dashboard panel are one question"
    );

    let next = match run(
        &fixture,
        Invocation::Next {
            count: 10,
            phase: None,
            claim: false,
        },
    )
    .unwrap()
    {
        Response::Stories { views, .. } => views
            .iter()
            .map(|view| view.story.id.clone())
            .collect::<Vec<_>>(),
        other => panic!("expected a list of stories, got {other:?}"),
    };
    assert_eq!(
        next.first().map(String::as_str),
        panel.first().copied(),
        "the TUI and `story next` must offer the same actionable head"
    );
    assert_eq!(
        next,
        [free, blocker, dependent],
        "multi-result `next` continues through the dependent it virtually unblocks"
    );
}

/// Helper: relate two stories, the way `story relate <a> <rel> <b>` does.
fn relate(fixture: &Fixture, a: &str, relation: &str, b: &str) {
    run(
        fixture,
        Invocation::Relate {
            a: a.to_string(),
            relation: relation.to_string(),
            b: b.to_string(),
            remove: false,
        },
    )
    .unwrap();
}

/// Helper: create an unpublished draft, which the snapshot carries beside
/// its stories rather than among them (SH-175).
fn create_draft(fixture: &Fixture, title: &str) -> String {
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
            draft: true,
        },
    )
    .unwrap()
    {
        Response::Story(view) => view.story.id,
        other => panic!("expected a story, got {other:?}"),
    }
}

// ─── Helper struct for TOML serialization of states ─────────────────
