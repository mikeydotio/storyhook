use crate::domain::{Priority, StateChanges, StoryEvent, SuperState};

/// A snapshot of a story's events before a mutation, used for undo/redo.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    /// Human-readable description of the action, e.g. "Moved SH-1 to in-progress"
    pub description: String,
    /// The story ID this entry pertains to
    pub story_id: String,
    /// The full event log of the story before the mutation was applied.
    /// Empty vec means the story did not exist yet (was newly created).
    pub events_before: Vec<StoryEvent>,
}

/// The primary view the TUI can display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Board,
    Graph,
}

/// Filter specification for narrowing displayed stories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterSpec {
    pub text: Option<String>,
    pub state: Option<String>,
    pub assignee: Option<String>,
    pub priority: Option<Priority>,
    pub label: Option<String>,
    /// Keep only stories `story list --blocked` would keep: open, and
    /// stopped by *something* — an `awaiting` reason, the `blocked` state,
    /// an unmet `blocked-by` edge, or an `obviated-by` one.
    pub blocked: bool,
    /// Keep only stories `story list --ready` would keep: unblocked, and not
    /// already claimed by someone.
    pub ready: bool,
}

/// All actions that can be dispatched through the TUI event loop.
#[derive(Debug, Clone)]
pub enum Action {
    // Navigation
    SwitchView(View),
    OpenDetail(String),
    OpenCreateForm,
    OpenStatesEditor,
    CloseModal,
    FocusFilterBar,
    UnfocusFilterBar,
    ToggleHelp,

    // Board
    ToggleSection(String),

    // Data mutations
    CreateStory {
        title: String,
        priority: Option<Priority>,
        labels: Vec<String>,
        assignee: Option<String>,
        description: Option<String>,
    },
    MoveStory {
        id: String,
        target_state: String,
    },
    UpdateTitle {
        id: String,
        title: String,
    },
    SetDescription {
        id: String,
        description: String,
    },
    SetPriority {
        id: String,
        priority: Priority,
    },
    SetLabels {
        id: String,
        labels: Vec<String>,
    },
    AssignStory {
        id: String,
        assignee: String,
    },
    AddComment {
        id: String,
        text: String,
    },
    SetAwaiting {
        id: String,
        reason: String,
    },
    ClearAwaiting {
        id: String,
    },

    // Project configuration (the statuses editor). Not undoable: the undo
    // stack replays one story's event log, and these edit the project's
    // state set — sometimes migrating stories as a side effect.
    AddState {
        slug: String,
        super_state: SuperState,
    },
    SetStateFields {
        slug: String,
        changes: StateChanges,
        /// Where the status's open stories go when this edit reclassifies
        /// them; required by `storage::update_state` in exactly that case.
        move_stories_to: Option<String>,
    },
    RemoveState {
        slug: String,
        move_stories_to: Option<String>,
    },
    ReorderStates {
        order: Vec<String>,
    },

    // Filtering
    SetFilter(FilterSpec),
    ClearFilter(usize),
    ClearAllFilters,

    // Undo/Redo
    Undo,
    Redo,

    // System
    RefreshData,
    Notify(String),
    OpenEditor {
        id: String,
    },
    Quit,
}
