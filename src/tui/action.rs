use crate::domain::{Priority, StoryEvent};

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
    pub blocked: bool,
    pub ready: bool,
}

/// All actions that can be dispatched through the TUI event loop.
#[derive(Debug, Clone)]
pub enum Action {
    // Navigation
    SwitchView(View),
    OpenDetail(String),
    OpenCreateForm,
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
