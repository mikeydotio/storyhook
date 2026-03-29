use std::time::Instant;

use super::action::{FilterSpec, UndoEntry, View};
use super::data::DataStore;
use super::focus::{FocusStack, FocusTarget};

/// Central application state for the TUI.
pub struct AppState {
    pub data: DataStore,
    pub focus: FocusStack,
    pub view: View,
    pub filters: Vec<FilterSpec>,
    pub filter_bar_focused: bool,
    pub running: bool,
    pub notification: Option<(String, Instant)>,
    pub terminal_size: (u16, u16),
    /// Session-only undo stack: most recent mutation is at the back.
    pub undo_stack: Vec<UndoEntry>,
    /// Session-only redo stack: populated when undo is performed.
    pub redo_stack: Vec<UndoEntry>,
}

impl AppState {
    /// Create a new AppState with sensible defaults.
    ///
    /// Starts on the Dashboard view with no filters, no modals, and running=true.
    pub fn new(data: DataStore) -> Self {
        Self {
            data,
            focus: FocusStack::new(FocusTarget::Dashboard),
            view: View::Dashboard,
            filters: Vec::new(),
            filter_bar_focused: false,
            running: true,
            notification: None,
            terminal_size: (0, 0),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }
}
