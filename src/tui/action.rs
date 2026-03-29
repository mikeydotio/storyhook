use crate::domain::Priority;

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
    },
    MoveStory {
        id: String,
        target_state: String,
    },
    UpdateTitle {
        id: String,
        title: String,
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

    // System
    RefreshData,
    Notify(String),
    OpenEditor { id: String },
    Quit,
}
