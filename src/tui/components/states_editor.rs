//! The statuses editor modal: per-repo configuration of the project's state
//! set, the TUI half of SH-41.
//!
//! Everything here edits the project's state catalog through the same service
//! calls `story state …` and the web dashboard use, so the three agree by
//! construction rather than by convention.

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tui_input::backend::crossterm::EventHandler;

use crate::domain::{FieldEdit, STATE_ROLE_ACTIVE, StateChanges, SuperState};
use crate::tui::action::Action;
use crate::tui::components::modal::render_modal;
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::Component;

/// The edit waiting on the user naming somewhere for a status's stories to
/// go — see [`EditorMode::Migrating`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingEdit {
    /// Flip the status between OPEN and CLOSED, reclassifying its stories.
    ToggleSuperState,
    /// Remove the status, leaving its stories without a definition.
    Delete,
}

/// What the editor is currently doing. Everything but `Browse` captures
/// typing, and `Esc` steps back out to `Browse` rather than closing the
/// modal — closing from inside a half-typed slug would be a trap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorMode {
    Browse,
    /// Typing the slug of a new status.
    NewSlug,
    /// Typing the selected status's description.
    EditDescription,
    /// Choosing where the selected status's open stories go before
    /// `pending` is applied.
    Migrating {
        pending: PendingEdit,
        cursor: usize,
    },
    /// A delete of an empty status, one keypress from happening.
    ConfirmDelete,
}

/// The statuses editor modal.
pub struct StatesEditor {
    pub cursor: usize,
    pub mode: EditorMode,
    /// Backs both `NewSlug` and `EditDescription` — only one can be active.
    pub input: tui_input::Input,
}

impl Default for StatesEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl StatesEditor {
    pub fn new() -> Self {
        Self {
            cursor: 0,
            mode: EditorMode::Browse,
            input: tui_input::Input::default(),
        }
    }

    /// How many open stories sit in `slug`.
    ///
    /// Archived stories aren't loaded into the TUI's `DataStore` and so
    /// aren't counted here; a delete they block is refused by
    /// `storage::remove_state`, which reports why.
    fn open_count(state: &AppState, slug: &str) -> usize {
        state
            .data
            .stories
            .iter()
            .filter(|story| !story.deleted && story.state == slug)
            .count()
    }

    /// Every status except the selected one — the destinations its stories
    /// could move to.
    fn destinations(state: &AppState, slug: &str) -> Vec<String> {
        state
            .data
            .states
            .iter()
            .filter(|def| def.slug != slug)
            .map(|def| def.slug.clone())
            .collect()
    }

    fn selected_slug(&self, state: &AppState) -> Option<String> {
        state
            .data
            .states
            .get(self.cursor)
            .map(|def| def.slug.clone())
    }

    /// Starts `pending`, asking for a destination first when the status
    /// still holds open stories — moving them is the whole reason either
    /// edit is allowed at all.
    fn begin(&mut self, state: &AppState, pending: PendingEdit) -> Vec<Action> {
        let Some(slug) = self.selected_slug(state) else {
            return vec![];
        };
        if Self::open_count(state, &slug) == 0 {
            return match pending {
                PendingEdit::ToggleSuperState => self.apply_toggle(state, None),
                PendingEdit::Delete => {
                    self.mode = EditorMode::ConfirmDelete;
                    vec![]
                }
            };
        }
        if Self::destinations(state, &slug).is_empty() {
            return vec![Action::Notify(format!(
                "`{slug}` holds stories and there is no other status to move them into"
            ))];
        }
        self.mode = EditorMode::Migrating { pending, cursor: 0 };
        vec![]
    }

    fn apply_toggle(&mut self, state: &AppState, move_stories_to: Option<String>) -> Vec<Action> {
        let Some(def) = state.data.states.get(self.cursor) else {
            return vec![];
        };
        let next = match def.super_state {
            SuperState::Open => SuperState::Closed,
            SuperState::Closed => SuperState::Open,
        };
        self.mode = EditorMode::Browse;
        vec![Action::SetStateFields {
            slug: def.slug.clone(),
            changes: StateChanges {
                super_state: Some(next),
                ..StateChanges::default()
            },
            move_stories_to,
        }]
    }

    /// Toggles `role = "active"` on the selected status. Clearing is always
    /// safe; setting it is refused downstream if another status already has
    /// it, since only one status can be the one work starts in.
    fn toggle_role(&self, state: &AppState) -> Vec<Action> {
        let Some(def) = state.data.states.get(self.cursor) else {
            return vec![];
        };
        let role = if def.role.as_deref() == Some(STATE_ROLE_ACTIVE) {
            FieldEdit::Clear
        } else {
            FieldEdit::Set(STATE_ROLE_ACTIVE.to_string())
        };
        vec![Action::SetStateFields {
            slug: def.slug.clone(),
            changes: StateChanges {
                role,
                ..StateChanges::default()
            },
            move_stories_to: None,
        }]
    }

    /// Moves the selected status `offset` places, and follows it with the
    /// cursor so the same status stays selected.
    fn reorder(&mut self, state: &AppState, offset: isize) -> Vec<Action> {
        let mut order: Vec<String> = state
            .data
            .states
            .iter()
            .map(|def| def.slug.clone())
            .collect();
        let target = self.cursor as isize + offset;
        if target < 0 || target as usize >= order.len() {
            return vec![];
        }
        let target = target as usize;
        order.swap(self.cursor, target);
        self.cursor = target;
        vec![Action::ReorderStates { order }]
    }

    fn submit_new_slug(&mut self) -> Vec<Action> {
        let slug = self.input.value().trim().to_string();
        self.mode = EditorMode::Browse;
        self.input.reset();
        if slug.is_empty() {
            return vec![Action::Notify("a slug is required".to_string())];
        }
        // New statuses start OPEN: a status is created to hold live work,
        // and `o` flips it in one keypress if not.
        vec![Action::AddState {
            slug,
            super_state: SuperState::Open,
        }]
    }

    fn submit_description(&mut self, state: &AppState) -> Vec<Action> {
        let Some(slug) = self.selected_slug(state) else {
            self.mode = EditorMode::Browse;
            return vec![];
        };
        let text = self.input.value().trim().to_string();
        self.mode = EditorMode::Browse;
        self.input.reset();
        vec![Action::SetStateFields {
            slug,
            changes: StateChanges {
                description: if text.is_empty() {
                    FieldEdit::Clear
                } else {
                    FieldEdit::Set(text)
                },
                ..StateChanges::default()
            },
            move_stories_to: None,
        }]
    }

    fn handle_browse_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        let count = state.data.states.len();
        match key.code {
            KeyCode::Esc => vec![Action::CloseModal],
            KeyCode::Char('j') | KeyCode::Down if self.cursor + 1 < count => {
                self.cursor += 1;
                vec![]
            }
            KeyCode::Char('k') | KeyCode::Up if self.cursor > 0 => {
                self.cursor -= 1;
                vec![]
            }
            KeyCode::Char('J') => self.reorder(state, 1),
            KeyCode::Char('K') => self.reorder(state, -1),
            KeyCode::Char('o') => self.begin(state, PendingEdit::ToggleSuperState),
            KeyCode::Char('a') => self.toggle_role(state),
            KeyCode::Char('e') => {
                if let Some(def) = state.data.states.get(self.cursor) {
                    self.input = tui_input::Input::new(def.description.clone().unwrap_or_default());
                    self.mode = EditorMode::EditDescription;
                }
                vec![]
            }
            KeyCode::Char('n') => {
                self.input.reset();
                self.mode = EditorMode::NewSlug;
                vec![]
            }
            KeyCode::Char('d') => self.begin(state, PendingEdit::Delete),
            _ => vec![],
        }
    }
}

impl Component for StatesEditor {
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        match self.mode.clone() {
            EditorMode::Browse => self.handle_browse_key(key, state),

            EditorMode::NewSlug => match key.code {
                KeyCode::Esc => {
                    self.mode = EditorMode::Browse;
                    self.input.reset();
                    vec![]
                }
                KeyCode::Enter => self.submit_new_slug(),
                _ => {
                    self.input.handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            },

            EditorMode::EditDescription => match key.code {
                KeyCode::Esc => {
                    self.mode = EditorMode::Browse;
                    self.input.reset();
                    vec![]
                }
                KeyCode::Enter => self.submit_description(state),
                _ => {
                    self.input.handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            },

            EditorMode::Migrating { pending, cursor } => {
                let Some(slug) = self.selected_slug(state) else {
                    self.mode = EditorMode::Browse;
                    return vec![];
                };
                let destinations = Self::destinations(state, &slug);
                match key.code {
                    KeyCode::Esc => {
                        self.mode = EditorMode::Browse;
                        vec![]
                    }
                    KeyCode::Char('j') | KeyCode::Down if cursor + 1 < destinations.len() => {
                        self.mode = EditorMode::Migrating {
                            pending,
                            cursor: cursor + 1,
                        };
                        vec![]
                    }
                    KeyCode::Char('k') | KeyCode::Up if cursor > 0 => {
                        self.mode = EditorMode::Migrating {
                            pending,
                            cursor: cursor - 1,
                        };
                        vec![]
                    }
                    KeyCode::Enter => {
                        let Some(destination) = destinations.get(cursor).cloned() else {
                            self.mode = EditorMode::Browse;
                            return vec![];
                        };
                        match pending {
                            PendingEdit::ToggleSuperState => {
                                self.apply_toggle(state, Some(destination))
                            }
                            PendingEdit::Delete => {
                                self.mode = EditorMode::Browse;
                                vec![Action::RemoveState {
                                    slug,
                                    move_stories_to: Some(destination),
                                }]
                            }
                        }
                    }
                    _ => vec![],
                }
            }

            EditorMode::ConfirmDelete => {
                self.mode = EditorMode::Browse;
                match key.code {
                    KeyCode::Char('y') => match self.selected_slug(state) {
                        Some(slug) => vec![Action::RemoveState {
                            slug,
                            move_stories_to: None,
                        }],
                        None => vec![],
                    },
                    _ => vec![],
                }
            }
        }
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent, _state: &AppState) -> Vec<Action> {
        vec![]
    }

    /// Keeps the cursor on a real row after states are added, removed, or
    /// reordered underneath it.
    fn on_state_change(&mut self, state: &AppState) {
        let count = state.data.states.len();
        if count == 0 {
            self.cursor = 0;
        } else if self.cursor >= count {
            self.cursor = count - 1;
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        let theme = Theme::from_env();
        let inner = render_modal(frame, area, "Statuses", &theme);

        // Short lines on purpose: the modal is two thirds of the terminal,
        // so anything much longer is clipped mid-word on a narrow one.
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                "Order sets the board's columns.",
                theme.status_bar,
            )),
            Line::from(Span::styled(
                "New stories start in the first open status.",
                theme.status_bar,
            )),
            Line::from(""),
        ];

        for (index, def) in state.data.states.iter().enumerate() {
            let selected = index == self.cursor;
            let open = StatesEditor::open_count(state, &def.slug);
            let mut row = format!(
                "{} {:<16} {:<7}",
                if selected { ">" } else { " " },
                def.slug,
                def.super_state.as_str()
            );
            row.push_str(&format!(
                "{:<8}",
                if def.role.as_deref() == Some(STATE_ROLE_ACTIVE) {
                    "active"
                } else {
                    ""
                }
            ));
            row.push_str(&format!("{open:>3} open"));
            if let Some(ref description) = def.description {
                row.push_str(&format!("  {description}"));
            }
            lines.push(Line::from(Span::styled(
                row,
                if selected {
                    theme.cursor
                } else {
                    ratatui::style::Style::default()
                },
            )));
        }

        if state.data.states.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no statuses configured",
                theme.status_bar,
            )));
        }

        lines.push(Line::from(""));
        lines.extend(self.render_prompt(state, &theme));

        frame.render_widget(Paragraph::new(lines), inner);
    }
}

impl StatesEditor {
    /// The mode-specific footer: the key hints while browsing, or whatever
    /// the current mode is waiting for.
    fn render_prompt<'a>(&self, state: &AppState, theme: &Theme) -> Vec<Line<'a>> {
        let key = |k: &'a str, label: &'a str| {
            vec![
                Span::styled(k, theme.status_bar_keys),
                Span::styled(label, theme.status_bar),
            ]
        };
        match &self.mode {
            // Two lines: the whole hint set on one runs past the modal's
            // inner width and gets clipped mid-word.
            EditorMode::Browse => {
                let line = |hints: &[(&'a str, &'a str)]| {
                    let mut spans = Vec::new();
                    for (k, label) in hints {
                        spans.extend(key(k, label));
                    }
                    Line::from(spans)
                };
                vec![
                    line(&[
                        ("j/k", " move  "),
                        ("J/K", " reorder  "),
                        ("o", " open/closed  "),
                        ("a", " active"),
                    ]),
                    line(&[
                        ("e", " describe  "),
                        ("n", " new  "),
                        ("d", " delete  "),
                        ("Esc", " close"),
                    ]),
                    Line::from(Span::styled(
                        "Status edits are not undoable.",
                        theme.status_bar,
                    )),
                ]
            }
            EditorMode::NewSlug => vec![Line::from(vec![
                Span::styled("New status: ", theme.story_title),
                Span::styled(self.input.value().to_string(), theme.cursor),
                Span::styled("   Enter", theme.status_bar_keys),
                Span::styled(" create  ", theme.status_bar),
                Span::styled("Esc", theme.status_bar_keys),
                Span::styled(" cancel", theme.status_bar),
            ])],
            EditorMode::EditDescription => vec![Line::from(vec![
                Span::styled("Description: ", theme.story_title),
                Span::styled(self.input.value().to_string(), theme.cursor),
                Span::styled("   Enter", theme.status_bar_keys),
                Span::styled(" save (empty clears)  ", theme.status_bar),
                Span::styled("Esc", theme.status_bar_keys),
                Span::styled(" cancel", theme.status_bar),
            ])],
            EditorMode::Migrating { pending, cursor } => {
                let slug = self.selected_slug(state).unwrap_or_default();
                let open = StatesEditor::open_count(state, &slug);
                let verb = match pending {
                    PendingEdit::ToggleSuperState => "reclassifying",
                    PendingEdit::Delete => "removing",
                };
                let mut lines = vec![Line::from(Span::styled(
                    format!(
                        "Move {open} {} out of `{slug}` before {verb} it — to:",
                        if open == 1 { "story" } else { "stories" }
                    ),
                    theme.story_title,
                ))];
                for (index, destination) in
                    StatesEditor::destinations(state, &slug).iter().enumerate()
                {
                    let closes = state
                        .data
                        .state_map
                        .get(destination)
                        .is_some_and(|def| def.super_state == SuperState::Closed);
                    lines.push(Line::from(Span::styled(
                        format!(
                            "  {} {destination}{}",
                            if index == *cursor { ">" } else { " " },
                            if closes { "  (closes them)" } else { "" }
                        ),
                        if index == *cursor {
                            theme.cursor
                        } else {
                            ratatui::style::Style::default()
                        },
                    )));
                }
                lines.push(Line::from(vec![
                    Span::styled("Enter", theme.status_bar_keys),
                    Span::styled(" move and apply  ", theme.status_bar),
                    Span::styled("Esc", theme.status_bar_keys),
                    Span::styled(" cancel", theme.status_bar),
                ]));
                lines
            }
            EditorMode::ConfirmDelete => {
                let slug = self.selected_slug(state).unwrap_or_default();
                vec![Line::from(vec![
                    Span::styled(format!("Delete `{slug}`? "), theme.story_title),
                    Span::styled("y", theme.status_bar_keys),
                    Span::styled(" yes  ", theme.status_bar),
                    Span::styled("any other key", theme.status_bar_keys),
                    Span::styled(" no", theme.status_bar),
                ])]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{StateDef, StorySnapshot};
    use crate::tui::data::DataStore;

    fn state_def(slug: &str, super_state: SuperState, role: Option<&str>) -> StateDef {
        StateDef {
            slug: slug.to_string(),
            super_state,
            role: role.map(str::to_string),
            description: None,
        }
    }

    fn snapshot(id: &str, state: &str) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: "A story".to_string(),
            created_at: "2026-07-01T00:00:00Z".to_string(),
            updated_at: "2026-07-01T00:00:00Z".to_string(),
            state: state.to_string(),
            state_computed: false,
            superstate: SuperState::Open,
            assignee: None,
            awaiting: None,
            comments: Vec::new(),
            referenced_by_commits: Vec::new(),
            relationships: Vec::new(),
            priority: crate::domain::Priority::None,
            priority_assessed: false,
            labels: Vec::new(),
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
            hidden_at: None,
            draft: false,
            attachments: Vec::new(),
            next_attachment_id: 1,
        }
    }

    /// Default fixture: todo (1 story), in-progress (empty, active), done.
    fn app_state() -> AppState {
        let states = vec![
            state_def("todo", SuperState::Open, None),
            state_def("in-progress", SuperState::Open, Some("active")),
            state_def("done", SuperState::Closed, None),
        ];
        AppState::new(DataStore::from_test_data(
            states,
            vec![snapshot("SH-1", "todo")],
            "SH".to_string(),
            Vec::new(),
        ))
    }

    fn press(editor: &mut StatesEditor, code: KeyCode, state: &AppState) -> Vec<Action> {
        editor.handle_key(KeyEvent::from(code), state)
    }

    #[test]
    fn cursor_moves_within_bounds() {
        let state = app_state();
        let mut editor = StatesEditor::new();

        press(&mut editor, KeyCode::Char('k'), &state);
        assert_eq!(editor.cursor, 0, "cursor must not go above the first row");

        for _ in 0..5 {
            press(&mut editor, KeyCode::Char('j'), &state);
        }
        assert_eq!(editor.cursor, 2, "cursor must stop at the last row");
    }

    #[test]
    fn shift_j_reorders_and_follows_the_status() {
        let state = app_state();
        let mut editor = StatesEditor::new();

        let actions = press(&mut editor, KeyCode::Char('J'), &state);
        match &actions[0] {
            Action::ReorderStates { order } => {
                assert_eq!(order, &["in-progress", "todo", "done"]);
            }
            other => panic!("expected a reorder, got {other:?}"),
        }
        assert_eq!(editor.cursor, 1, "the cursor follows the status it moved");
    }

    #[test]
    fn reorder_at_the_edges_does_nothing() {
        let state = app_state();
        let mut editor = StatesEditor::new();
        assert!(press(&mut editor, KeyCode::Char('K'), &state).is_empty());

        editor.cursor = 2;
        assert!(press(&mut editor, KeyCode::Char('J'), &state).is_empty());
    }

    #[test]
    fn o_on_an_empty_status_toggles_immediately() {
        let state = app_state();
        let mut editor = StatesEditor::new();
        editor.cursor = 1; // in-progress, no stories

        let actions = press(&mut editor, KeyCode::Char('o'), &state);
        match &actions[0] {
            Action::SetStateFields {
                slug,
                changes,
                move_stories_to,
            } => {
                assert_eq!(slug, "in-progress");
                assert_eq!(changes.super_state, Some(SuperState::Closed));
                assert_eq!(move_stories_to.as_deref(), None);
            }
            other => panic!("expected a field set, got {other:?}"),
        }
    }

    /// The whole point of the prompt: an occupied status can't be
    /// reclassified until its stories have somewhere to go.
    #[test]
    fn o_on_an_occupied_status_asks_where_the_stories_go() {
        let state = app_state();
        let mut editor = StatesEditor::new(); // todo, 1 story

        assert!(press(&mut editor, KeyCode::Char('o'), &state).is_empty());
        assert!(matches!(
            editor.mode,
            EditorMode::Migrating {
                pending: PendingEdit::ToggleSuperState,
                cursor: 0
            }
        ));

        let actions = press(&mut editor, KeyCode::Enter, &state);
        match &actions[0] {
            Action::SetStateFields {
                slug,
                move_stories_to,
                ..
            } => {
                assert_eq!(slug, "todo");
                assert_eq!(move_stories_to.as_deref(), Some("in-progress"));
            }
            other => panic!("expected a field set, got {other:?}"),
        }
        assert_eq!(editor.mode, EditorMode::Browse);
    }

    #[test]
    fn the_destination_list_is_selectable_and_cancellable() {
        let state = app_state();
        let mut editor = StatesEditor::new();
        press(&mut editor, KeyCode::Char('d'), &state);

        press(&mut editor, KeyCode::Char('j'), &state);
        let actions = press(&mut editor, KeyCode::Enter, &state);
        match &actions[0] {
            Action::RemoveState {
                slug,
                move_stories_to,
            } => {
                assert_eq!(slug, "todo");
                assert_eq!(move_stories_to.as_deref(), Some("done"));
            }
            other => panic!("expected a removal, got {other:?}"),
        }

        press(&mut editor, KeyCode::Char('d'), &state);
        assert!(press(&mut editor, KeyCode::Esc, &state).is_empty());
        assert_eq!(editor.mode, EditorMode::Browse);
    }

    #[test]
    fn deleting_an_empty_status_takes_a_confirmation() {
        let state = app_state();
        let mut editor = StatesEditor::new();
        editor.cursor = 1; // in-progress, no stories

        assert!(press(&mut editor, KeyCode::Char('d'), &state).is_empty());
        assert_eq!(editor.mode, EditorMode::ConfirmDelete);

        let actions = press(&mut editor, KeyCode::Char('y'), &state);
        match &actions[0] {
            Action::RemoveState {
                slug,
                move_stories_to,
            } => {
                assert_eq!(slug, "in-progress");
                assert!(move_stories_to.is_none());
            }
            other => panic!("expected a removal, got {other:?}"),
        }
    }

    #[test]
    fn any_other_key_declines_the_delete() {
        let state = app_state();
        let mut editor = StatesEditor::new();
        editor.cursor = 1;
        press(&mut editor, KeyCode::Char('d'), &state);

        assert!(press(&mut editor, KeyCode::Char('n'), &state).is_empty());
        assert_eq!(editor.mode, EditorMode::Browse);
    }

    #[test]
    fn a_toggles_the_active_role_both_ways() {
        let state = app_state();
        let mut editor = StatesEditor::new();

        let actions = press(&mut editor, KeyCode::Char('a'), &state);
        match &actions[0] {
            Action::SetStateFields { slug, changes, .. } => {
                assert_eq!(slug, "todo");
                assert_eq!(changes.role, FieldEdit::Set("active".to_string()));
            }
            other => panic!("expected a field set, got {other:?}"),
        }

        editor.cursor = 1; // in-progress already carries the role
        let actions = press(&mut editor, KeyCode::Char('a'), &state);
        match &actions[0] {
            Action::SetStateFields { changes, .. } => {
                assert_eq!(changes.role, FieldEdit::Clear);
            }
            other => panic!("expected a field set, got {other:?}"),
        }
    }

    #[test]
    fn n_creates_an_open_status_and_rejects_an_empty_slug() {
        let state = app_state();
        let mut editor = StatesEditor::new();

        press(&mut editor, KeyCode::Char('n'), &state);
        assert_eq!(editor.mode, EditorMode::NewSlug);
        for ch in "review".chars() {
            press(&mut editor, KeyCode::Char(ch), &state);
        }
        let actions = press(&mut editor, KeyCode::Enter, &state);
        match &actions[0] {
            Action::AddState { slug, super_state } => {
                assert_eq!(slug, "review");
                assert_eq!(*super_state, SuperState::Open);
            }
            other => panic!("expected an add, got {other:?}"),
        }
        assert_eq!(editor.mode, EditorMode::Browse);

        press(&mut editor, KeyCode::Char('n'), &state);
        let actions = press(&mut editor, KeyCode::Enter, &state);
        assert!(matches!(actions[0], Action::Notify(_)));
    }

    /// `e` on a status with a description starts from that text — the field
    /// is edited, not retyped.
    #[test]
    fn e_edits_the_existing_description_and_empty_clears_it() {
        let mut state = app_state();
        state.data.states[0].description = Some("Not started yet".to_string());
        let mut editor = StatesEditor::new();

        press(&mut editor, KeyCode::Char('e'), &state);
        assert_eq!(editor.input.value(), "Not started yet");

        for _ in 0.."Not started yet".len() {
            press(&mut editor, KeyCode::Backspace, &state);
        }
        let actions = press(&mut editor, KeyCode::Enter, &state);
        match &actions[0] {
            Action::SetStateFields { changes, .. } => {
                assert_eq!(changes.description, FieldEdit::Clear);
            }
            other => panic!("expected a field set, got {other:?}"),
        }
    }

    /// Esc inside a sub-mode steps back to browsing; only Esc while browsing
    /// closes the modal, so a half-typed slug can't be lost by one keypress.
    #[test]
    fn esc_backs_out_one_level_at_a_time() {
        let state = app_state();
        let mut editor = StatesEditor::new();

        press(&mut editor, KeyCode::Char('n'), &state);
        assert!(press(&mut editor, KeyCode::Esc, &state).is_empty());
        assert_eq!(editor.mode, EditorMode::Browse);

        let actions = press(&mut editor, KeyCode::Esc, &state);
        assert!(matches!(actions[0], Action::CloseModal));
    }

    #[test]
    fn cursor_is_clamped_when_states_disappear() {
        let mut state = app_state();
        let mut editor = StatesEditor::new();
        editor.cursor = 2;

        state.data.states.pop();
        editor.on_state_change(&state);
        assert_eq!(editor.cursor, 1);

        state.data.states.clear();
        editor.on_state_change(&state);
        assert_eq!(editor.cursor, 0);
    }

    /// A one-status project has nowhere to migrate to, so the edit is
    /// refused with a reason rather than opening an empty picker.
    #[test]
    fn an_occupied_status_with_no_alternative_reports_why() {
        let states = vec![state_def("todo", SuperState::Open, None)];
        let state = AppState::new(DataStore::from_test_data(
            states,
            vec![snapshot("SH-1", "todo")],
            "SH".to_string(),
            Vec::new(),
        ));
        let mut editor = StatesEditor::new();

        let actions = press(&mut editor, KeyCode::Char('d'), &state);
        assert!(matches!(actions[0], Action::Notify(_)));
        assert_eq!(editor.mode, EditorMode::Browse);
    }
}
