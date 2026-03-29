use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;

use crate::domain::{Priority, SuperState};
use crate::tui::action::Action;
use crate::tui::components::modal::render_modal;
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::Component;

/// Fields that can be selected/edited in the story detail view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailField {
    Title,
    State,
    Priority,
    Assignee,
    Labels,
    Awaiting,
    Relationships,
    Comments,
}

const FIELDS: &[DetailField] = &[
    DetailField::Title,
    DetailField::State,
    DetailField::Priority,
    DetailField::Assignee,
    DetailField::Labels,
    DetailField::Awaiting,
    DetailField::Relationships,
    DetailField::Comments,
];

/// The editing mode for the story detail component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailMode {
    Viewing,
    EditingTitle,
    EditingPriority,
    EditingLabels,
    EditingAssignee,
    EditingAwaiting,
    AddingComment,
}

/// Story detail modal component.
pub struct StoryDetail {
    pub story_id: String,
    pub mode: DetailMode,
    pub selected_field: usize,
    pub title_input: tui_input::Input,
    pub label_input: tui_input::Input,
    pub assignee_input: tui_input::Input,
    pub awaiting_input: tui_input::Input,
    pub comment_input: tui_input::Input,
    pub priority_cursor: usize,
    pub scroll_offset: u16,
}

const PRIORITY_OPTIONS: &[Priority] = &[
    Priority::None,
    Priority::Low,
    Priority::Medium,
    Priority::High,
    Priority::Critical,
];

impl StoryDetail {
    pub fn new(story_id: String) -> Self {
        Self {
            story_id,
            mode: DetailMode::Viewing,
            selected_field: 0,
            title_input: tui_input::Input::default(),
            label_input: tui_input::Input::default(),
            assignee_input: tui_input::Input::default(),
            awaiting_input: tui_input::Input::default(),
            comment_input: tui_input::Input::default(),
            priority_cursor: 0,
            scroll_offset: 0,
        }
    }

    /// Determine the next state for a story (for > key).
    fn next_state_for_story(&self, state: &AppState) -> Option<String> {
        let story = state.data.find_story(&self.story_id)?;

        let open_states: Vec<&str> = state
            .data
            .states
            .iter()
            .filter(|s| s.super_state == SuperState::Open)
            .map(|s| s.slug.as_str())
            .collect();

        let current_idx = open_states.iter().position(|s| *s == story.state)?;
        if current_idx + 1 < open_states.len() {
            Some(open_states[current_idx + 1].to_string())
        } else {
            // At last OPEN state; move to first CLOSED state
            state
                .data
                .states
                .iter()
                .find(|s| s.super_state == SuperState::Closed)
                .map(|s| s.slug.clone())
        }
    }

    /// Determine the previous state for a story (for < key).
    fn prev_state_for_story(&self, state: &AppState) -> Option<String> {
        let story = state.data.find_story(&self.story_id)?;
        let open_states: Vec<&str> = state
            .data
            .states
            .iter()
            .filter(|s| s.super_state == SuperState::Open)
            .map(|s| s.slug.as_str())
            .collect();

        let current_idx = open_states.iter().position(|s| *s == story.state)?;
        if current_idx > 0 {
            Some(open_states[current_idx - 1].to_string())
        } else {
            None
        }
    }

    fn handle_viewing_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        // Ctrl+E: open $EDITOR for adding a comment
        if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return vec![Action::OpenEditor {
                id: self.story_id.clone(),
            }];
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.selected_field + 1 < FIELDS.len() {
                    self.selected_field += 1;
                }
                vec![]
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.selected_field > 0 {
                    self.selected_field -= 1;
                }
                vec![]
            }
            KeyCode::Char('e') => {
                let story = match state.data.find_story(&self.story_id) {
                    Some(s) => s,
                    None => return vec![],
                };
                match FIELDS[self.selected_field] {
                    DetailField::Title => {
                        self.title_input = tui_input::Input::new(story.title.clone());
                        self.mode = DetailMode::EditingTitle;
                    }
                    DetailField::Priority => {
                        self.priority_cursor = PRIORITY_OPTIONS
                            .iter()
                            .position(|p| p == &story.priority)
                            .unwrap_or(0);
                        self.mode = DetailMode::EditingPriority;
                    }
                    DetailField::Labels => {
                        self.label_input = tui_input::Input::new(story.labels.join(", "));
                        self.mode = DetailMode::EditingLabels;
                    }
                    DetailField::Assignee => {
                        self.assignee_input =
                            tui_input::Input::new(story.assignee.clone().unwrap_or_default());
                        self.mode = DetailMode::EditingAssignee;
                    }
                    DetailField::Awaiting => {
                        if story.awaiting.is_some() {
                            // Clear awaiting
                            return vec![Action::ClearAwaiting {
                                id: self.story_id.clone(),
                            }];
                        } else {
                            self.awaiting_input = tui_input::Input::default();
                            self.mode = DetailMode::EditingAwaiting;
                        }
                    }
                    // State, Relationships, Comments not editable via 'e'
                    _ => {}
                }
                vec![]
            }
            KeyCode::Char('c') => {
                self.comment_input = tui_input::Input::default();
                self.mode = DetailMode::AddingComment;
                vec![]
            }
            KeyCode::Char('>') => {
                if let Some(target) = self.next_state_for_story(state) {
                    vec![Action::MoveStory {
                        id: self.story_id.clone(),
                        target_state: target,
                    }]
                } else {
                    vec![]
                }
            }
            KeyCode::Char('<') => {
                if let Some(target) = self.prev_state_for_story(state) {
                    vec![Action::MoveStory {
                        id: self.story_id.clone(),
                        target_state: target,
                    }]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> Vec<Action> {
        match &self.mode {
            DetailMode::EditingTitle => match key.code {
                KeyCode::Enter => {
                    let title = self.title_input.value().to_string();
                    self.mode = DetailMode::Viewing;
                    if title.is_empty() {
                        return vec![];
                    }
                    vec![Action::UpdateTitle {
                        id: self.story_id.clone(),
                        title,
                    }]
                }
                KeyCode::Esc => {
                    self.mode = DetailMode::Viewing;
                    vec![]
                }
                _ => {
                    self.title_input
                        .handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            },
            DetailMode::EditingPriority => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    if self.priority_cursor + 1 < PRIORITY_OPTIONS.len() {
                        self.priority_cursor += 1;
                    }
                    vec![]
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if self.priority_cursor > 0 {
                        self.priority_cursor -= 1;
                    }
                    vec![]
                }
                KeyCode::Enter => {
                    let priority = PRIORITY_OPTIONS[self.priority_cursor].clone();
                    self.mode = DetailMode::Viewing;
                    vec![Action::SetPriority {
                        id: self.story_id.clone(),
                        priority,
                    }]
                }
                KeyCode::Esc => {
                    self.mode = DetailMode::Viewing;
                    vec![]
                }
                _ => vec![],
            },
            DetailMode::EditingLabels => match key.code {
                KeyCode::Enter => {
                    let raw = self.label_input.value().to_string();
                    let labels: Vec<String> = raw
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    self.mode = DetailMode::Viewing;
                    vec![Action::SetLabels {
                        id: self.story_id.clone(),
                        labels,
                    }]
                }
                KeyCode::Esc => {
                    self.mode = DetailMode::Viewing;
                    vec![]
                }
                _ => {
                    self.label_input
                        .handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            },
            DetailMode::EditingAssignee => match key.code {
                KeyCode::Enter => {
                    let assignee = self.assignee_input.value().to_string();
                    self.mode = DetailMode::Viewing;
                    if assignee.is_empty() {
                        return vec![];
                    }
                    vec![Action::AssignStory {
                        id: self.story_id.clone(),
                        assignee,
                    }]
                }
                KeyCode::Esc => {
                    self.mode = DetailMode::Viewing;
                    vec![]
                }
                _ => {
                    self.assignee_input
                        .handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            },
            DetailMode::EditingAwaiting => match key.code {
                KeyCode::Enter => {
                    let reason = self.awaiting_input.value().to_string();
                    self.mode = DetailMode::Viewing;
                    if reason.is_empty() {
                        return vec![];
                    }
                    vec![Action::SetAwaiting {
                        id: self.story_id.clone(),
                        reason,
                    }]
                }
                KeyCode::Esc => {
                    self.mode = DetailMode::Viewing;
                    vec![]
                }
                _ => {
                    self.awaiting_input
                        .handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            },
            DetailMode::AddingComment => match key.code {
                KeyCode::Enter => {
                    let text = self.comment_input.value().to_string();
                    self.mode = DetailMode::Viewing;
                    if text.is_empty() {
                        return vec![];
                    }
                    vec![Action::AddComment {
                        id: self.story_id.clone(),
                        text,
                    }]
                }
                KeyCode::Esc => {
                    self.mode = DetailMode::Viewing;
                    vec![]
                }
                _ => {
                    self.comment_input
                        .handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            },
            DetailMode::Viewing => unreachable!(),
        }
    }
}

impl Component for StoryDetail {
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        if self.mode == DetailMode::Viewing {
            self.handle_viewing_key(key, state)
        } else {
            self.handle_editing_key(key)
        }
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent, _state: &AppState) -> Vec<Action> {
        vec![]
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        let theme = Theme::from_env();
        let inner = render_modal(frame, area, &self.story_id, &theme);

        let story = match state.data.find_story(&self.story_id) {
            Some(s) => s,
            None => {
                let msg = Line::from(Span::raw("Story not found"));
                frame.render_widget(Paragraph::new(msg), inner);
                return;
            }
        };

        let label_width = 15;
        let mut lines: Vec<Line> = Vec::new();

        // Title
        let title_line = render_field(
            "Title",
            &story.title,
            self.selected_field == 0,
            label_width,
            &theme,
        );
        if self.mode == DetailMode::EditingTitle && self.selected_field == 0 {
            lines.push(render_editing_field(
                "Title",
                self.title_input.value(),
                label_width,
                &theme,
            ));
        } else {
            lines.push(title_line);
        }

        // State
        lines.push(render_field(
            "State",
            &story.state,
            self.selected_field == 1,
            label_width,
            &theme,
        ));

        // Priority
        if self.mode == DetailMode::EditingPriority && self.selected_field == 2 {
            lines.push(render_label_span("Priority", true, label_width, &theme));
            for (i, p) in PRIORITY_OPTIONS.iter().enumerate() {
                let marker = if i == self.priority_cursor { "> " } else { "  " };
                let style = if i == self.priority_cursor {
                    theme.cursor
                } else {
                    theme.status_bar
                };
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(label_width)),
                    Span::styled(format!("{marker}{}", p.as_str()), style),
                ]));
            }
        } else {
            lines.push(render_field(
                "Priority",
                story.priority.as_str(),
                self.selected_field == 2,
                label_width,
                &theme,
            ));
        }

        // Assignee
        let assignee_val = story.assignee.as_deref().unwrap_or("(none)");
        if self.mode == DetailMode::EditingAssignee && self.selected_field == 3 {
            lines.push(render_editing_field(
                "Assignee",
                self.assignee_input.value(),
                label_width,
                &theme,
            ));
        } else {
            lines.push(render_field(
                "Assignee",
                assignee_val,
                self.selected_field == 3,
                label_width,
                &theme,
            ));
        }

        // Labels
        let labels_val = if story.labels.is_empty() {
            "(none)".to_string()
        } else {
            story.labels.join(", ")
        };
        if self.mode == DetailMode::EditingLabels && self.selected_field == 4 {
            lines.push(render_editing_field(
                "Labels",
                self.label_input.value(),
                label_width,
                &theme,
            ));
        } else {
            lines.push(render_field(
                "Labels",
                &labels_val,
                self.selected_field == 4,
                label_width,
                &theme,
            ));
        }

        // Awaiting
        let awaiting_val = story.awaiting.as_deref().unwrap_or("(none)");
        if self.mode == DetailMode::EditingAwaiting && self.selected_field == 5 {
            lines.push(render_editing_field(
                "Awaiting",
                self.awaiting_input.value(),
                label_width,
                &theme,
            ));
        } else {
            lines.push(render_field(
                "Awaiting",
                awaiting_val,
                self.selected_field == 5,
                label_width,
                &theme,
            ));
        }

        // Relationships
        let rels_val = if story.relationships.is_empty() {
            "(none)".to_string()
        } else {
            story
                .relationships
                .iter()
                .map(|r| format!("{} {}", r.relation, r.other_id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(render_field(
            "Relations",
            &rels_val,
            self.selected_field == 6,
            label_width,
            &theme,
        ));

        // Comments
        let is_comments_selected = self.selected_field == 7;
        lines.push(render_label_span(
            "Comments",
            is_comments_selected,
            label_width,
            &theme,
        ));

        if self.mode == DetailMode::AddingComment {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(label_width)),
                Span::styled(
                    format!("> {}", self.comment_input.value()),
                    theme.cursor,
                ),
            ]));
        }

        if story.comments.is_empty() {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(label_width)),
                Span::styled("(no comments)", theme.section_count),
            ]));
        } else {
            for comment in &story.comments {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(label_width)),
                    Span::styled(&comment.at[..10.min(comment.at.len())], theme.story_id),
                    Span::raw(" "),
                    Span::raw(comment.text.clone()),
                ]));
            }
        }

        // Timestamps
        lines.push(Line::from(""));
        lines.push(render_field(
            "Created",
            &story.created_at,
            false,
            label_width,
            &theme,
        ));
        lines.push(render_field(
            "Updated",
            &story.updated_at,
            false,
            label_width,
            &theme,
        ));

        // Apply scroll offset for comments section
        let visible_height = inner.height as usize;
        let total_lines = lines.len();
        let scroll = if total_lines > visible_height {
            let max_scroll = total_lines.saturating_sub(visible_height);
            (self.scroll_offset as usize).min(max_scroll)
        } else {
            0
        };

        let visible_lines: Vec<Line> = lines.into_iter().skip(scroll).take(visible_height).collect();

        frame.render_widget(Paragraph::new(visible_lines), inner);
    }
}

/// Render a single field row with a dimmed label and value.
fn render_field<'a>(
    label: &str,
    value: &str,
    selected: bool,
    label_width: usize,
    theme: &Theme,
) -> Line<'a> {
    let padded_label = format!("{:<width$}", label, width = label_width);
    let label_style = if selected {
        theme.story_title
    } else {
        theme.section_count
    };
    let value_style = if selected {
        theme.story_title
    } else {
        ratatui::style::Style::default()
    };

    Line::from(vec![
        Span::styled(padded_label, label_style),
        Span::styled(value.to_string(), value_style),
    ])
}

/// Render a label-only span (used before multi-line content).
fn render_label_span<'a>(
    label: &str,
    selected: bool,
    label_width: usize,
    theme: &Theme,
) -> Line<'a> {
    let padded_label = format!("{:<width$}", label, width = label_width);
    let style = if selected {
        theme.story_title
    } else {
        theme.section_count
    };
    Line::from(Span::styled(padded_label, style))
}

/// Render an editing field with input value.
fn render_editing_field<'a>(
    label: &str,
    value: &str,
    label_width: usize,
    theme: &Theme,
) -> Line<'a> {
    let padded_label = format!("{:<width$}", label, width = label_width);
    Line::from(vec![
        Span::styled(padded_label, theme.story_title),
        Span::styled(format!("> {value}"), theme.cursor),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Priority, StateDef, StoryComment, StorySnapshot, SuperState};
    use crate::tui::action::View;
    use crate::tui::data::DataStore;
    use crate::tui::focus::{FocusStack, FocusTarget};
    use crate::tui::state::AppState;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn test_states() -> Vec<StateDef> {
        vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
            },
            StateDef {
                slug: "in-progress".to_string(),
                super_state: SuperState::Open,
                role: Some("active".to_string()),
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
            },
        ]
    }

    fn test_snapshot() -> StorySnapshot {
        StorySnapshot {
            id: "SH-1".to_string(),
            title: "Test story".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: "todo".to_string(),
            superstate: SuperState::Open,
            assignee: Some("mikey".to_string()),
            awaiting: None,
            comments: vec![
                StoryComment {
                    at: "2026-01-02T00:00:00Z".to_string(),
                    text: "First comment".to_string(),
                },
            ],
            relationships: vec![],
            priority: Priority::High,
            labels: vec!["bug".to_string(), "tui".to_string()],
            closed_at: None,
        }
    }

    fn make_state(stories: Vec<StorySnapshot>) -> AppState {
        let data = DataStore::from_test_data(
            test_states(),
            stories,
            "SH".to_string(),
            vec![],
        );
        AppState {
            data,
            focus: FocusStack::new(FocusTarget::Board),
            view: View::Board,
            filters: Vec::new(),
            filter_bar_focused: false,
            running: true,
            notification: None,
            terminal_size: (120, 40),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // --- Task 3.2 Tests: Field Navigation ---

    #[test]
    fn field_navigation_j_moves_down() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        assert_eq!(detail.selected_field, 0);

        detail.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(detail.selected_field, 1);

        detail.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(detail.selected_field, 2);
    }

    #[test]
    fn field_navigation_k_moves_up() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 3;

        detail.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(detail.selected_field, 2);

        detail.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(detail.selected_field, 1);
    }

    #[test]
    fn field_navigation_bounds() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());

        // At top, k stays at 0
        detail.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(detail.selected_field, 0);

        // At bottom, j stays at last
        detail.selected_field = FIELDS.len() - 1;
        detail.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(detail.selected_field, FIELDS.len() - 1);
    }

    // --- Task 3.3 Tests: Editing Mode Transitions ---

    #[test]
    fn e_on_title_enters_editing_mode() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 0; // Title

        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingTitle);
        assert_eq!(detail.title_input.value(), "Test story");
    }

    #[test]
    fn esc_cancels_editing() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 0;

        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingTitle);

        let actions = detail.handle_key(key(KeyCode::Esc), &state);
        assert_eq!(detail.mode, DetailMode::Viewing);
        assert!(actions.is_empty());
    }

    #[test]
    fn enter_confirms_title_edit() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 0;

        detail.handle_key(key(KeyCode::Char('e')), &state);
        // The input is pre-filled with "Test story"
        let actions = detail.handle_key(key(KeyCode::Enter), &state);

        assert_eq!(detail.mode, DetailMode::Viewing);
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::UpdateTitle { id, title } if id == "SH-1" && title == "Test story")
        );
    }

    #[test]
    fn e_on_priority_shows_cycle_selector() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 2; // Priority

        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingPriority);
        // Should be positioned on High (current priority)
        assert_eq!(
            PRIORITY_OPTIONS[detail.priority_cursor],
            Priority::High
        );
    }

    #[test]
    fn priority_cycle_j_k() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 2;
        detail.handle_key(key(KeyCode::Char('e')), &state);

        let start = detail.priority_cursor;

        detail.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(detail.priority_cursor, start + 1);

        detail.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(detail.priority_cursor, start);
    }

    #[test]
    fn c_opens_comment_input() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());

        detail.handle_key(key(KeyCode::Char('c')), &state);
        assert_eq!(detail.mode, DetailMode::AddingComment);
    }

    #[test]
    fn move_story_forward_from_detail() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());

        let actions = detail.handle_key(
            KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT),
            &state,
        );
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::MoveStory { id, target_state } if id == "SH-1" && target_state == "in-progress")
        );
    }

    #[test]
    fn e_on_awaiting_clears_when_set() {
        let mut snap = test_snapshot();
        snap.awaiting = Some("blocked by API".to_string());
        let state = make_state(vec![snap]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 5; // Awaiting

        let actions = detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::ClearAwaiting { id } if id == "SH-1"));
        // Mode stays Viewing because ClearAwaiting is immediate
        assert_eq!(detail.mode, DetailMode::Viewing);
    }

    // =======================================================================
    // QA: Story not found edge case
    // =======================================================================

    #[test]
    fn viewing_key_on_nonexistent_story_does_nothing() {
        // If the story was deleted externally while the modal is open
        let state = make_state(vec![test_snapshot()]); // SH-1 exists
        let mut detail = StoryDetail::new("SH-99".to_string()); // SH-99 doesn't

        // 'e' on Title should bail gracefully (story not found)
        detail.selected_field = 0;
        let actions = detail.handle_key(key(KeyCode::Char('e')), &state);
        assert!(actions.is_empty());
        assert_eq!(detail.mode, DetailMode::Viewing);

        // '>' should bail gracefully
        let actions = detail.handle_key(
            KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT),
            &state,
        );
        assert!(actions.is_empty());

        // '<' should bail gracefully
        let actions = detail.handle_key(
            KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT),
            &state,
        );
        assert!(actions.is_empty());
    }

    // =======================================================================
    // QA: Empty title rejection on edit
    // =======================================================================

    #[test]
    fn enter_on_empty_title_does_not_emit_update() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 0;
        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingTitle);

        // Clear the pre-filled input
        for _ in 0..20 {
            detail.handle_key(key(KeyCode::Backspace), &state);
        }
        assert!(detail.title_input.value().is_empty());

        // Enter with empty title should NOT emit UpdateTitle
        let actions = detail.handle_key(key(KeyCode::Enter), &state);
        assert!(actions.is_empty(), "Empty title should be rejected");
        assert_eq!(detail.mode, DetailMode::Viewing);
    }

    // =======================================================================
    // QA: Empty comment rejection
    // =======================================================================

    #[test]
    fn enter_on_empty_comment_does_not_emit_add() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());

        detail.handle_key(key(KeyCode::Char('c')), &state);
        assert_eq!(detail.mode, DetailMode::AddingComment);

        // Enter immediately (empty comment)
        let actions = detail.handle_key(key(KeyCode::Enter), &state);
        assert!(actions.is_empty(), "Empty comment should be rejected");
        assert_eq!(detail.mode, DetailMode::Viewing);
    }

    // =======================================================================
    // QA: Empty assignee rejection
    // =======================================================================

    #[test]
    fn enter_on_empty_assignee_does_not_emit_assign() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 3; // Assignee

        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingAssignee);

        // Clear the pre-filled input ("mikey")
        for _ in 0..10 {
            detail.handle_key(key(KeyCode::Backspace), &state);
        }

        let actions = detail.handle_key(key(KeyCode::Enter), &state);
        assert!(actions.is_empty(), "Empty assignee should be rejected");
    }

    // =======================================================================
    // QA: Empty awaiting rejection
    // =======================================================================

    #[test]
    fn enter_on_empty_awaiting_does_not_emit_set() {
        let mut snap = test_snapshot();
        snap.awaiting = None; // not awaiting
        let state = make_state(vec![snap]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 5; // Awaiting

        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingAwaiting);

        // Enter immediately (empty reason)
        let actions = detail.handle_key(key(KeyCode::Enter), &state);
        assert!(actions.is_empty(), "Empty awaiting reason should be rejected");
    }

    // =======================================================================
    // QA: State movement boundary -- can't move past first/last
    // =======================================================================

    #[test]
    fn move_backward_at_first_state_does_nothing() {
        let state = make_state(vec![test_snapshot()]); // SH-1 in "todo" (first open state)
        let mut detail = StoryDetail::new("SH-1".to_string());

        let actions = detail.handle_key(
            KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT),
            &state,
        );
        assert!(actions.is_empty(), "Can't move backward from first state");
    }

    #[test]
    fn move_forward_from_last_open_goes_to_closed() {
        let mut snap = test_snapshot();
        snap.state = "in-progress".to_string(); // last open state before "done"
        let state = make_state(vec![snap]);
        let mut detail = StoryDetail::new("SH-1".to_string());

        let actions = detail.handle_key(
            KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT),
            &state,
        );
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::MoveStory { target_state, .. } if target_state == "done")
        );
    }

    // =======================================================================
    // QA: Priority cycle boundary
    // =======================================================================

    #[test]
    fn priority_cursor_does_not_go_below_zero() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 2; // Priority
        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingPriority);

        // Move to first option
        detail.priority_cursor = 0;
        // Try to move up -- should stay at 0
        detail.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(detail.priority_cursor, 0);
    }

    #[test]
    fn priority_cursor_does_not_exceed_max() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 2;
        detail.handle_key(key(KeyCode::Char('e')), &state);

        // Move to last option
        detail.priority_cursor = PRIORITY_OPTIONS.len() - 1;
        detail.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(detail.priority_cursor, PRIORITY_OPTIONS.len() - 1);
    }

    // =======================================================================
    // QA: Esc during each editing mode returns to Viewing
    // =======================================================================

    #[test]
    fn esc_from_every_editing_mode_returns_to_viewing() {
        let state = make_state(vec![test_snapshot()]);

        // EditingPriority
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 2;
        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingPriority);
        detail.handle_key(key(KeyCode::Esc), &state);
        assert_eq!(detail.mode, DetailMode::Viewing);

        // EditingLabels
        detail.selected_field = 4;
        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingLabels);
        detail.handle_key(key(KeyCode::Esc), &state);
        assert_eq!(detail.mode, DetailMode::Viewing);

        // EditingAssignee
        detail.selected_field = 3;
        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingAssignee);
        detail.handle_key(key(KeyCode::Esc), &state);
        assert_eq!(detail.mode, DetailMode::Viewing);

        // EditingAwaiting
        detail.selected_field = 5;
        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::EditingAwaiting);
        detail.handle_key(key(KeyCode::Esc), &state);
        assert_eq!(detail.mode, DetailMode::Viewing);

        // AddingComment
        detail.handle_key(key(KeyCode::Char('c')), &state);
        assert_eq!(detail.mode, DetailMode::AddingComment);
        detail.handle_key(key(KeyCode::Esc), &state);
        assert_eq!(detail.mode, DetailMode::Viewing);
    }

    // =======================================================================
    // QA: e on non-editable fields does nothing
    // =======================================================================

    #[test]
    fn e_on_state_field_does_nothing() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 1; // State

        detail.handle_key(key(KeyCode::Char('e')), &state);
        // Should remain in Viewing mode (State is not editable via e)
        assert_eq!(detail.mode, DetailMode::Viewing);
    }

    #[test]
    fn e_on_relationships_does_nothing() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 6; // Relationships

        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::Viewing);
    }

    #[test]
    fn e_on_comments_does_nothing() {
        let state = make_state(vec![test_snapshot()]);
        let mut detail = StoryDetail::new("SH-1".to_string());
        detail.selected_field = 7; // Comments

        detail.handle_key(key(KeyCode::Char('e')), &state);
        assert_eq!(detail.mode, DetailMode::Viewing);
    }
}
