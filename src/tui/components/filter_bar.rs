use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;

use crate::domain::Priority;
use crate::tui::action::{Action, FilterSpec};
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::{Component, HitRegion, HitTarget};

/// The filter bar component: renders a 1-line bar above the board.
///
/// Shows active filter chips and a text input when focused.
/// When unfocused, shows chips plus a dim "Press / to filter" hint.
pub struct FilterBar {
    pub input: tui_input::Input,
    pub hit_regions: Vec<HitRegion>,
    /// The render area from the last render, for mouse detection.
    pub render_area: Rect,
}

impl Default for FilterBar {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterBar {
    pub fn new() -> Self {
        Self {
            input: tui_input::Input::default(),
            hit_regions: Vec::new(),
            render_area: Rect::default(),
        }
    }
}

impl Component for FilterBar {
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        // The filter bar only handles keys when focused.
        // Esc and Ctrl+U are handled by the keymap layer.
        if !state.filter_bar_focused {
            return vec![];
        }

        match key.code {
            KeyCode::Enter => {
                let raw = self.input.value().trim().to_string();
                self.input.reset();
                if raw.is_empty() {
                    return vec![Action::UnfocusFilterBar];
                }
                match parse_filter(&raw) {
                    Some(spec) => vec![Action::SetFilter(spec)],
                    None => vec![Action::Notify(format!("Unknown filter: {raw}"))],
                }
            }
            KeyCode::Backspace => {
                if self.input.value().is_empty() {
                    // Remove last chip
                    if !state.filters.is_empty() {
                        vec![Action::ClearFilter(state.filters.len() - 1)]
                    } else {
                        vec![]
                    }
                } else {
                    self.input
                        .handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            }
            KeyCode::Tab => {
                // Autocomplete: skip for now (bonus)
                vec![]
            }
            _ => {
                self.input
                    .handle_event(&crossterm::event::Event::Key(key));
                vec![]
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, state: &AppState) -> Vec<Action> {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return vec![];
        }

        // Check if click is within the filter bar area
        if mouse.row < self.render_area.y
            || mouse.row >= self.render_area.y + self.render_area.height
            || mouse.column < self.render_area.x
            || mouse.column >= self.render_area.x + self.render_area.width
        {
            return vec![];
        }

        // Check if click is on a filter chip
        for region in &self.hit_regions {
            if mouse.column >= region.rect.x
                && mouse.column < region.rect.x + region.rect.width
                && mouse.row >= region.rect.y
                && mouse.row < region.rect.y + region.rect.height
                && let HitTarget::FilterChip { index } = &region.target
                && *index < state.filters.len()
            {
                return vec![Action::ClearFilter(*index)];
            }
        }

        // Click anywhere else on filter bar focuses it
        if !state.filter_bar_focused {
            return vec![Action::FocusFilterBar];
        }

        vec![]
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        self.render_area = area;
        self.hit_regions.clear();
        let theme = Theme::from_env();

        let mut spans: Vec<Span> = Vec::new();
        let mut x_offset: u16 = 0;

        // Render active filter chips and record their hit regions
        for (idx, filter) in state.filters.iter().enumerate() {
            let chip_text = format_chip(filter);
            let display = format!(" {chip_text} ");
            let chip_width = display.chars().count() as u16;
            self.hit_regions.push(HitRegion {
                rect: Rect::new(area.x + x_offset, area.y, chip_width, 1),
                target: HitTarget::FilterChip { index: idx },
            });
            x_offset += chip_width;
            spans.push(Span::styled(display, theme.filter_chip));
            spans.push(Span::raw(" "));
            x_offset += 1;
        }

        if state.filter_bar_focused {
            // Show input cursor
            let input_value = self.input.value().to_string();
            if input_value.is_empty() {
                spans.push(Span::styled("type to filter...", theme.section_count));
            } else {
                spans.push(Span::styled(input_value, theme.story_title));
            }
            spans.push(Span::styled("\u{2588}", theme.cursor)); // block cursor
        } else if state.filters.is_empty() {
            // Show dim hint
            spans.push(Span::styled("Press / to filter", theme.section_count));
        }

        let line = Line::from(spans);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn hit_regions(&self) -> &[HitRegion] {
        &self.hit_regions
    }
}

/// Parse a filter string into a FilterSpec.
///
/// Supported formats:
/// - `state:todo` or `s:todo` -> state filter
/// - `priority:high` or `p:high` -> priority filter
/// - `assignee:mikey` or `@mikey` or `a:mikey` -> assignee filter
/// - `label:bug` or `#bug` or `l:bug` -> label filter
/// - `blocked` -> blocked filter
/// - `ready` -> ready filter
/// - plain text -> text substring search
pub fn parse_filter(raw: &str) -> Option<FilterSpec> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Check for shorthand prefixes first
    if let Some(value) = trimmed.strip_prefix('@')
        && !value.is_empty()
    {
        return Some(FilterSpec {
            assignee: Some(value.to_string()),
            ..Default::default()
        });
    }

    if let Some(value) = trimmed.strip_prefix('#')
        && !value.is_empty()
    {
        return Some(FilterSpec {
            label: Some(value.to_string()),
            ..Default::default()
        });
    }

    // Check for keyword filters
    let lower = trimmed.to_ascii_lowercase();
    if lower == "blocked" {
        return Some(FilterSpec {
            blocked: true,
            ..Default::default()
        });
    }
    if lower == "ready" {
        return Some(FilterSpec {
            ready: true,
            ..Default::default()
        });
    }

    // Check for prefix:value patterns
    if let Some((prefix, value)) = trimmed.split_once(':') {
        let prefix_lower = prefix.to_ascii_lowercase();
        let value = value.trim().to_string();
        if value.is_empty() {
            // Treat as plain text search if value is empty
            return Some(FilterSpec {
                text: Some(trimmed.to_string()),
                ..Default::default()
            });
        }

        match prefix_lower.as_str() {
            "state" | "s" => {
                return Some(FilterSpec {
                    state: Some(value),
                    ..Default::default()
                });
            }
            "priority" | "p" => {
                if let Some(priority) = Priority::parse(&value) {
                    return Some(FilterSpec {
                        priority: Some(priority),
                        ..Default::default()
                    });
                }
                // Unknown priority: treat as text search
                return Some(FilterSpec {
                    text: Some(trimmed.to_string()),
                    ..Default::default()
                });
            }
            "assignee" | "a" => {
                return Some(FilterSpec {
                    assignee: Some(value),
                    ..Default::default()
                });
            }
            "label" | "l" => {
                return Some(FilterSpec {
                    label: Some(value),
                    ..Default::default()
                });
            }
            _ => {
                // Unknown prefix: treat the whole thing as text search
                return Some(FilterSpec {
                    text: Some(trimmed.to_string()),
                    ..Default::default()
                });
            }
        }
    }

    // Plain text search (no prefix, no shorthand)
    Some(FilterSpec {
        text: Some(trimmed.to_string()),
        ..Default::default()
    })
}

/// Format a FilterSpec as a short, readable chip label.
pub fn format_chip(spec: &FilterSpec) -> String {
    if let Some(ref state) = spec.state {
        return format!("state:{state}");
    }
    if let Some(ref priority) = spec.priority {
        return format!("p:{}", priority.as_str());
    }
    if let Some(ref assignee) = spec.assignee {
        return format!("@{assignee}");
    }
    if let Some(ref label) = spec.label {
        return format!("#{label}");
    }
    if spec.blocked {
        return "blocked".to_string();
    }
    if spec.ready {
        return "ready".to_string();
    }
    if let Some(ref text) = spec.text {
        return format!("\"{text}\"");
    }
    "?".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Task 4.1: FilterSpec parsing tests ---

    #[test]
    fn parse_state_filter_full_prefix() {
        let spec = parse_filter("state:todo").unwrap();
        assert_eq!(spec.state, Some("todo".to_string()));
        assert!(spec.text.is_none());
    }

    #[test]
    fn parse_state_filter_short_prefix() {
        let spec = parse_filter("s:in-progress").unwrap();
        assert_eq!(spec.state, Some("in-progress".to_string()));
    }

    #[test]
    fn parse_priority_filter() {
        let spec = parse_filter("priority:high").unwrap();
        assert_eq!(spec.priority, Some(Priority::High));
    }

    #[test]
    fn parse_priority_filter_short() {
        let spec = parse_filter("p:critical").unwrap();
        assert_eq!(spec.priority, Some(Priority::Critical));
    }

    #[test]
    fn parse_assignee_filter_at_sign() {
        let spec = parse_filter("@mikey").unwrap();
        assert_eq!(spec.assignee, Some("mikey".to_string()));
    }

    #[test]
    fn parse_assignee_filter_prefix() {
        let spec = parse_filter("a:mikey").unwrap();
        assert_eq!(spec.assignee, Some("mikey".to_string()));
    }

    #[test]
    fn parse_assignee_filter_full_prefix() {
        let spec = parse_filter("assignee:bob").unwrap();
        assert_eq!(spec.assignee, Some("bob".to_string()));
    }

    #[test]
    fn parse_label_filter_hash() {
        let spec = parse_filter("#bug").unwrap();
        assert_eq!(spec.label, Some("bug".to_string()));
    }

    #[test]
    fn parse_label_filter_prefix() {
        let spec = parse_filter("l:feature").unwrap();
        assert_eq!(spec.label, Some("feature".to_string()));
    }

    #[test]
    fn parse_label_filter_full_prefix() {
        let spec = parse_filter("label:tui").unwrap();
        assert_eq!(spec.label, Some("tui".to_string()));
    }

    #[test]
    fn parse_blocked_filter() {
        let spec = parse_filter("blocked").unwrap();
        assert!(spec.blocked);
        assert!(!spec.ready);
    }

    #[test]
    fn parse_ready_filter() {
        let spec = parse_filter("ready").unwrap();
        assert!(spec.ready);
        assert!(!spec.blocked);
    }

    #[test]
    fn parse_blocked_case_insensitive() {
        let spec = parse_filter("BLOCKED").unwrap();
        assert!(spec.blocked);
    }

    #[test]
    fn parse_plain_text_filter() {
        let spec = parse_filter("login flow").unwrap();
        assert_eq!(spec.text, Some("login flow".to_string()));
    }

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse_filter("").is_none());
        assert!(parse_filter("  ").is_none());
    }

    #[test]
    fn parse_unknown_prefix_becomes_text() {
        let spec = parse_filter("foo:bar").unwrap();
        assert_eq!(spec.text, Some("foo:bar".to_string()));
    }

    #[test]
    fn parse_priority_case_insensitive() {
        let spec = parse_filter("P:HIGH").unwrap();
        assert_eq!(spec.priority, Some(Priority::High));
    }

    // --- Chip formatting tests ---

    #[test]
    fn format_chip_state() {
        let spec = FilterSpec {
            state: Some("todo".to_string()),
            ..Default::default()
        };
        assert_eq!(format_chip(&spec), "state:todo");
    }

    #[test]
    fn format_chip_assignee() {
        let spec = FilterSpec {
            assignee: Some("mikey".to_string()),
            ..Default::default()
        };
        assert_eq!(format_chip(&spec), "@mikey");
    }

    #[test]
    fn format_chip_label() {
        let spec = FilterSpec {
            label: Some("bug".to_string()),
            ..Default::default()
        };
        assert_eq!(format_chip(&spec), "#bug");
    }

    #[test]
    fn format_chip_text() {
        let spec = FilterSpec {
            text: Some("login".to_string()),
            ..Default::default()
        };
        assert_eq!(format_chip(&spec), "\"login\"");
    }

    // --- Task 5.3 Tests: Mouse support on filter bar ---

    #[test]
    fn mouse_click_on_chip_removes_filter() {
        use crate::tui::action::View;
        use crate::tui::data::DataStore;
        use crate::tui::focus::{FocusStack, FocusTarget};
        use crate::tui::state::AppState;

        let data = DataStore::from_test_data(vec![], vec![], "SH".to_string(), vec![]);
        let state = AppState {
            data,
            focus: FocusStack::new(FocusTarget::Board),
            view: View::Board,
            filters: vec![
                FilterSpec {
                    state: Some("todo".to_string()),
                    ..Default::default()
                },
                FilterSpec {
                    label: Some("bug".to_string()),
                    ..Default::default()
                },
            ],
            filter_bar_focused: false,
            running: true,
            notification: None,
            terminal_size: (80, 24),
        };

        let mut bar = FilterBar::new();
        // Simulate the hit regions as they would be after render
        // Chip 0: " state:todo " = 12 chars wide at x=0
        // Chip 1: " #bug " = 6 chars wide at x=13 (12 + 1 gap)
        bar.render_area = Rect::new(0, 0, 80, 1);
        bar.hit_regions = vec![
            HitRegion {
                rect: Rect::new(0, 0, 12, 1),
                target: HitTarget::FilterChip { index: 0 },
            },
            HitRegion {
                rect: Rect::new(13, 0, 6, 1),
                target: HitTarget::FilterChip { index: 1 },
            },
        ];

        // Click on first chip
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let actions = bar.handle_mouse(mouse, &state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::ClearFilter(0)));

        // Click on second chip
        let mouse2 = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 15,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let actions2 = bar.handle_mouse(mouse2, &state);
        assert_eq!(actions2.len(), 1);
        assert!(matches!(&actions2[0], Action::ClearFilter(1)));
    }

    #[test]
    fn mouse_click_on_bar_focuses_it() {
        use crate::tui::action::View;
        use crate::tui::data::DataStore;
        use crate::tui::focus::{FocusStack, FocusTarget};
        use crate::tui::state::AppState;

        let data = DataStore::from_test_data(vec![], vec![], "SH".to_string(), vec![]);
        let state = AppState {
            data,
            focus: FocusStack::new(FocusTarget::Board),
            view: View::Board,
            filters: vec![],
            filter_bar_focused: false,
            running: true,
            notification: None,
            terminal_size: (80, 24),
        };

        let mut bar = FilterBar::new();
        bar.render_area = Rect::new(0, 0, 80, 1);

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let actions = bar.handle_mouse(mouse, &state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::FocusFilterBar));
    }
}
