use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;

use crate::domain::Priority;
use crate::tui::action::Action;
use crate::tui::components::modal::render_modal;
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::Component;

/// Fields in the create form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateField {
    Title,
    Priority,
    Labels,
    Assignee,
}

const CREATE_FIELDS: &[CreateField] = &[
    CreateField::Title,
    CreateField::Priority,
    CreateField::Labels,
    CreateField::Assignee,
];

const PRIORITY_OPTIONS: &[Priority] = &[
    Priority::None,
    Priority::Low,
    Priority::Medium,
    Priority::High,
    Priority::Critical,
];

/// Create story form modal component.
pub struct CreateForm {
    pub focused_field: usize,
    pub title_input: tui_input::Input,
    pub label_input: tui_input::Input,
    pub assignee_input: tui_input::Input,
    pub priority_cursor: usize,
}

impl Default for CreateForm {
    fn default() -> Self {
        Self::new()
    }
}

impl CreateForm {
    pub fn new() -> Self {
        Self {
            focused_field: 0,
            title_input: tui_input::Input::default(),
            label_input: tui_input::Input::default(),
            assignee_input: tui_input::Input::default(),
            priority_cursor: 0,
        }
    }

    fn current_field(&self) -> CreateField {
        CREATE_FIELDS[self.focused_field]
    }

    fn submit(&self) -> Vec<Action> {
        let title = self.title_input.value().trim().to_string();
        if title.is_empty() {
            return vec![Action::Notify(
                "Title is required".to_string(),
            )];
        }

        let priority = if self.priority_cursor == 0 {
            None
        } else {
            Some(PRIORITY_OPTIONS[self.priority_cursor].clone())
        };

        let labels_raw = self.label_input.value().to_string();
        let labels: Vec<String> = labels_raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let assignee_raw = self.assignee_input.value().trim().to_string();
        let assignee = if assignee_raw.is_empty() {
            None
        } else {
            Some(assignee_raw)
        };

        vec![Action::CreateStory {
            title,
            priority,
            labels,
            assignee,
        }]
    }
}

impl Component for CreateForm {
    fn handle_key(&mut self, key: KeyEvent, _state: &AppState) -> Vec<Action> {
        match key.code {
            KeyCode::Tab => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+Tab: previous field
                    if self.focused_field > 0 {
                        self.focused_field -= 1;
                    } else {
                        self.focused_field = CREATE_FIELDS.len() - 1;
                    }
                } else {
                    // Tab: next field
                    self.focused_field = (self.focused_field + 1) % CREATE_FIELDS.len();
                }
                vec![]
            }
            KeyCode::BackTab => {
                // Shift+Tab via BackTab
                if self.focused_field > 0 {
                    self.focused_field -= 1;
                } else {
                    self.focused_field = CREATE_FIELDS.len() - 1;
                }
                vec![]
            }
            KeyCode::Enter => self.submit(),
            _ => {
                // Route to the focused field
                match self.current_field() {
                    CreateField::Title => {
                        self.title_input
                            .handle_event(&crossterm::event::Event::Key(key));
                    }
                    CreateField::Priority => match key.code {
                        KeyCode::Char('j') | KeyCode::Down => {
                            if self.priority_cursor + 1 < PRIORITY_OPTIONS.len() {
                                self.priority_cursor += 1;
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if self.priority_cursor > 0 {
                                self.priority_cursor -= 1;
                            }
                        }
                        _ => {}
                    },
                    CreateField::Labels => {
                        self.label_input
                            .handle_event(&crossterm::event::Event::Key(key));
                    }
                    CreateField::Assignee => {
                        self.assignee_input
                            .handle_event(&crossterm::event::Event::Key(key));
                    }
                }
                vec![]
            }
        }
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent, _state: &AppState) -> Vec<Action> {
        vec![]
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _state: &AppState) {
        let theme = Theme::from_env();
        let inner = render_modal(frame, area, "New Story", &theme);

        let label_width = 12;
        let mut lines: Vec<Line> = Vec::new();

        // Title
        lines.push(render_form_field(
            "Title",
            self.title_input.value(),
            self.focused_field == 0,
            label_width,
            &theme,
        ));

        // Priority
        let pri_label = PRIORITY_OPTIONS[self.priority_cursor].as_str();
        if self.focused_field == 1 {
            // Show all options
            for (i, p) in PRIORITY_OPTIONS.iter().enumerate() {
                let marker = if i == self.priority_cursor { "> " } else { "  " };
                let label = if i == 0 {
                    format!("{:<width$}", "Priority", width = label_width)
                } else {
                    " ".repeat(label_width)
                };
                let style = if i == self.priority_cursor {
                    theme.cursor
                } else {
                    theme.status_bar
                };
                lines.push(Line::from(vec![
                    Span::styled(label, theme.story_title),
                    Span::styled(format!("{marker}{}", p.as_str()), style),
                ]));
            }
        } else {
            lines.push(render_form_field(
                "Priority",
                pri_label,
                false,
                label_width,
                &theme,
            ));
        }

        // Labels
        lines.push(render_form_field(
            "Labels",
            self.label_input.value(),
            self.focused_field == 2,
            label_width,
            &theme,
        ));

        // Assignee
        lines.push(render_form_field(
            "Assignee",
            self.assignee_input.value(),
            self.focused_field == 3,
            label_width,
            &theme,
        ));

        // Help text
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Tab", theme.status_bar_keys),
            Span::styled(" next field  ", theme.status_bar),
            Span::styled("Enter", theme.status_bar_keys),
            Span::styled(" create  ", theme.status_bar),
            Span::styled("Esc", theme.status_bar_keys),
            Span::styled(" cancel", theme.status_bar),
        ]));

        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// Render a form field with label and value.
fn render_form_field<'a>(
    label: &str,
    value: &str,
    focused: bool,
    label_width: usize,
    theme: &Theme,
) -> Line<'a> {
    let padded_label = format!("{:<width$}", label, width = label_width);
    let label_style = if focused {
        theme.story_title
    } else {
        theme.section_count
    };

    let value_display = if focused {
        format!("> {value}")
    } else if value.is_empty() {
        "(empty)".to_string()
    } else {
        value.to_string()
    };

    let value_style = if focused {
        theme.cursor
    } else {
        ratatui::style::Style::default()
    };

    Line::from(vec![
        Span::styled(padded_label, label_style),
        Span::styled(value_display, value_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{StateDef, SuperState};
    use crate::tui::action::View;
    use crate::tui::data::DataStore;
    use crate::tui::focus::{FocusStack, FocusTarget};
    use crate::tui::state::AppState;

    fn make_state() -> AppState {
        let data = DataStore::from_test_data(
            vec![
                StateDef {
                    slug: "todo".to_string(),
                    super_state: SuperState::Open,
                    role: None,
                },
                StateDef {
                    slug: "done".to_string(),
                    super_state: SuperState::Closed,
                    role: None,
                },
            ],
            vec![],
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
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // --- Task 3.5 Tests: CreateForm ---

    #[test]
    fn tab_cycles_fields_forward() {
        let state = make_state();
        let mut form = CreateForm::new();
        assert_eq!(form.focused_field, 0); // Title

        form.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(form.focused_field, 1); // Priority

        form.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(form.focused_field, 2); // Labels

        form.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(form.focused_field, 3); // Assignee

        // Wraps around
        form.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(form.focused_field, 0); // Title
    }

    #[test]
    fn shift_tab_cycles_fields_backward() {
        let state = make_state();
        let mut form = CreateForm::new();
        assert_eq!(form.focused_field, 0);

        // BackTab goes to last field
        form.handle_key(key(KeyCode::BackTab), &state);
        assert_eq!(form.focused_field, 3); // Assignee

        form.handle_key(key(KeyCode::BackTab), &state);
        assert_eq!(form.focused_field, 2); // Labels
    }

    #[test]
    fn submit_with_empty_title_notifies() {
        let state = make_state();
        let mut form = CreateForm::new();

        let actions = form.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::Notify(msg) if msg.contains("Title")));
    }

    #[test]
    fn submit_with_title_creates_story() {
        let state = make_state();
        let mut form = CreateForm::new();

        // Type a title
        for ch in "My new story".chars() {
            form.handle_key(key(KeyCode::Char(ch)), &state);
        }

        let actions = form.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::CreateStory { title, priority, labels, assignee }
                if title == "My new story" && priority.is_none() && labels.is_empty() && assignee.is_none())
        );
    }

    #[test]
    fn priority_cycling_in_create_form() {
        let state = make_state();
        let mut form = CreateForm::new();

        // Move to priority field
        form.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(form.focused_field, 1);

        // Default is None (index 0)
        assert_eq!(form.priority_cursor, 0);

        // j moves down
        form.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(form.priority_cursor, 1);
        assert_eq!(PRIORITY_OPTIONS[form.priority_cursor], Priority::Low);

        form.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(form.priority_cursor, 2);
        assert_eq!(PRIORITY_OPTIONS[form.priority_cursor], Priority::Medium);

        // k moves back
        form.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(form.priority_cursor, 1);
    }

    #[test]
    fn submit_with_all_fields() {
        let state = make_state();
        let mut form = CreateForm::new();

        // Title
        for ch in "Full story".chars() {
            form.handle_key(key(KeyCode::Char(ch)), &state);
        }

        // Priority: move to field and set High
        form.handle_key(key(KeyCode::Tab), &state);
        form.handle_key(key(KeyCode::Char('j')), &state); // Low
        form.handle_key(key(KeyCode::Char('j')), &state); // Medium
        form.handle_key(key(KeyCode::Char('j')), &state); // High

        // Labels
        form.handle_key(key(KeyCode::Tab), &state);
        for ch in "bug, tui".chars() {
            form.handle_key(key(KeyCode::Char(ch)), &state);
        }

        // Assignee
        form.handle_key(key(KeyCode::Tab), &state);
        for ch in "mikey".chars() {
            form.handle_key(key(KeyCode::Char(ch)), &state);
        }

        let actions = form.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::CreateStory {
                title,
                priority,
                labels,
                assignee,
            } => {
                assert_eq!(title, "Full story");
                assert_eq!(priority, &Some(Priority::High));
                assert_eq!(labels, &vec!["bug".to_string(), "tui".to_string()]);
                assert_eq!(assignee, &Some("mikey".to_string()));
            }
            other => panic!("Expected CreateStory, got {other:?}"),
        }
    }

    // =======================================================================
    // QA: Empty title validation (the design spec says "if title non-empty")
    // =======================================================================

    #[test]
    fn submit_whitespace_only_title_notifies() {
        let state = make_state();
        let mut form = CreateForm::new();

        // Type only spaces
        for ch in "   ".chars() {
            form.handle_key(key(KeyCode::Char(ch)), &state);
        }

        let actions = form.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::Notify(msg) if msg.contains("Title")),
            "Whitespace-only title should be rejected"
        );
    }

    // =======================================================================
    // QA: Priority cursor boundaries
    // =======================================================================

    #[test]
    fn priority_cursor_cannot_go_below_zero() {
        let state = make_state();
        let mut form = CreateForm::new();
        form.handle_key(key(KeyCode::Tab), &state); // move to Priority field
        assert_eq!(form.priority_cursor, 0);

        // k at 0 stays at 0
        form.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(form.priority_cursor, 0);
    }

    #[test]
    fn priority_cursor_cannot_exceed_max() {
        let state = make_state();
        let mut form = CreateForm::new();
        form.handle_key(key(KeyCode::Tab), &state); // move to Priority field

        // Move to last option
        for _ in 0..10 {
            form.handle_key(key(KeyCode::Char('j')), &state);
        }
        assert_eq!(form.priority_cursor, PRIORITY_OPTIONS.len() - 1);
    }

    // =======================================================================
    // QA: Labels parsing edge cases
    // =======================================================================

    #[test]
    fn submit_with_trailing_commas_in_labels() {
        let state = make_state();
        let mut form = CreateForm::new();

        // Type title
        for ch in "Test".chars() {
            form.handle_key(key(KeyCode::Char(ch)), &state);
        }

        // Move to labels
        form.handle_key(key(KeyCode::Tab), &state); // Priority
        form.handle_key(key(KeyCode::Tab), &state); // Labels

        // Type labels with trailing/extra commas
        for ch in "bug,,, tui, ".chars() {
            form.handle_key(key(KeyCode::Char(ch)), &state);
        }

        let actions = form.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::CreateStory { labels, .. } => {
                // Empty strings from extra commas should be filtered out
                assert_eq!(labels, &vec!["bug".to_string(), "tui".to_string()]);
            }
            other => panic!("Expected CreateStory, got {other:?}"),
        }
    }

    // =======================================================================
    // QA: Tab wrapping
    // =======================================================================

    #[test]
    fn tab_wraps_from_last_to_first() {
        let state = make_state();
        let mut form = CreateForm::new();
        form.focused_field = CREATE_FIELDS.len() - 1;

        form.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(form.focused_field, 0);
    }

    #[test]
    fn backtab_wraps_from_first_to_last() {
        let state = make_state();
        let mut form = CreateForm::new();
        assert_eq!(form.focused_field, 0);

        form.handle_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
            &state,
        );
        assert_eq!(form.focused_field, CREATE_FIELDS.len() - 1);
    }

    // =======================================================================
    // QA: Submit from any field (not just title)
    // =======================================================================

    #[test]
    fn submit_works_from_assignee_field() {
        let state = make_state();
        let mut form = CreateForm::new();

        // Type title
        for ch in "My story".chars() {
            form.handle_key(key(KeyCode::Char(ch)), &state);
        }

        // Move to assignee (last field)
        form.handle_key(key(KeyCode::Tab), &state);
        form.handle_key(key(KeyCode::Tab), &state);
        form.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(form.focused_field, 3);

        // Submit should still work
        let actions = form.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::CreateStory { title, .. } if title == "My story"));
    }
}
