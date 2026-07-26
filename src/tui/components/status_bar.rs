use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::action::{Action, View};
use crate::tui::focus::Modal;
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::Component;

/// Bottom status bar: key hints (left), notification (center), view+count (right).
///
/// Passive component: handles no events, emits no actions.
pub struct StatusBar;

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBar {
    pub fn new() -> Self {
        Self
    }
}

impl Component for StatusBar {
    fn handle_key(&mut self, _key: KeyEvent, _state: &AppState) -> Vec<Action> {
        vec![]
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent, _state: &AppState) -> Vec<Action> {
        vec![]
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        let theme = Theme::from_env();
        let width = area.width as usize;

        // Left: context-sensitive key hints
        let hints = build_key_hints(state, &theme);

        // Center: notification
        let notification_spans = if let Some((ref msg, _)) = state.notification {
            vec![
                Span::raw("  "),
                Span::styled(
                    msg.clone(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]
        } else {
            vec![]
        };

        // Right: undo/redo indicator + view label + story count
        let view_label = match state.view {
            View::Dashboard => "Dashboard",
            View::Board => "Board",
            View::Graph => "Graph",
        };
        let count = state.data.story_count();
        let undo_redo_text = {
            let u = state.undo_stack.len();
            let r = state.redo_stack.len();
            if u > 0 || r > 0 {
                format!("u:{u} r:{r} | ")
            } else {
                String::new()
            }
        };
        let right_text = format!("{undo_redo_text}{view_label} | {count} stories");
        let right_len = right_text.chars().count();

        // Calculate spacing
        let hints_len: usize = hints.iter().map(|s| s.content.chars().count()).sum();
        let notif_len: usize = notification_spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum();

        let used = hints_len + notif_len + right_len;
        let gap = width.saturating_sub(used);

        let mut spans = hints;
        spans.extend(notification_spans);
        spans.push(Span::styled(" ".repeat(gap), theme.status_bar));
        spans.push(Span::styled(right_text, theme.status_bar));

        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

/// Build context-sensitive key hint spans for the status bar.
fn build_key_hints<'a>(state: &AppState, theme: &Theme) -> Vec<Span<'a>> {
    // Check if a modal is open
    if let Some(modal) = state.focus.top_modal() {
        return match modal {
            Modal::StoryDetail { .. } => vec![
                Span::styled(" Esc", theme.status_bar_keys),
                Span::styled(" close  ", theme.status_bar),
                Span::styled("j/k", theme.status_bar_keys),
                Span::styled(" nav  ", theme.status_bar),
                Span::styled("e", theme.status_bar_keys),
                Span::styled(" edit  ", theme.status_bar),
                Span::styled(">/<", theme.status_bar_keys),
                Span::styled(" move", theme.status_bar),
            ],
            Modal::CreateForm => vec![
                Span::styled(" Esc", theme.status_bar_keys),
                Span::styled(" cancel  ", theme.status_bar),
                Span::styled("Tab", theme.status_bar_keys),
                Span::styled(" next  ", theme.status_bar),
                Span::styled("Enter", theme.status_bar_keys),
                Span::styled(" submit", theme.status_bar),
            ],
            // The editor draws its own, mode-specific hints; this is the
            // one-line summary for the bar underneath it.
            Modal::StatesEditor => vec![
                Span::styled(" Esc", theme.status_bar_keys),
                Span::styled(" close  ", theme.status_bar),
                Span::styled("J/K", theme.status_bar_keys),
                Span::styled(" reorder  ", theme.status_bar),
                Span::styled("o", theme.status_bar_keys),
                Span::styled(" open/closed  ", theme.status_bar),
                Span::styled("n/d", theme.status_bar_keys),
                Span::styled(" new/delete", theme.status_bar),
            ],
            Modal::Help => vec![
                Span::styled(" Esc", theme.status_bar_keys),
                Span::styled(" close  ", theme.status_bar),
                Span::styled("j/k", theme.status_bar_keys),
                Span::styled(" scroll", theme.status_bar),
            ],
        };
    }

    if state.filter_bar_focused {
        return vec![
            Span::styled(" Esc", theme.status_bar_keys),
            Span::styled(" cancel  ", theme.status_bar),
            Span::styled("Enter", theme.status_bar_keys),
            Span::styled(" apply  ", theme.status_bar),
            Span::styled("C-u", theme.status_bar_keys),
            Span::styled(" clear", theme.status_bar),
        ];
    }

    // Base view hints
    match state.view {
        View::Board => vec![
            Span::styled(" q", theme.status_bar_keys),
            Span::styled(" quit  ", theme.status_bar),
            Span::styled("j/k", theme.status_bar_keys),
            Span::styled(" nav  ", theme.status_bar),
            Span::styled("Enter", theme.status_bar_keys),
            Span::styled(" open  ", theme.status_bar),
            Span::styled(">/<", theme.status_bar_keys),
            Span::styled(" move  ", theme.status_bar),
            Span::styled("n", theme.status_bar_keys),
            Span::styled(" new  ", theme.status_bar),
            Span::styled("?", theme.status_bar_keys),
            Span::styled(" help", theme.status_bar),
        ],
        View::Dashboard => vec![
            Span::styled(" q", theme.status_bar_keys),
            Span::styled(" quit  ", theme.status_bar),
            Span::styled("?", theme.status_bar_keys),
            Span::styled(" help  ", theme.status_bar),
            Span::styled("1", theme.status_bar_keys),
            Span::styled(" dash  ", theme.status_bar),
            Span::styled("2", theme.status_bar_keys),
            Span::styled(" board  ", theme.status_bar),
            Span::styled("3", theme.status_bar_keys),
            Span::styled(" graph", theme.status_bar),
        ],
        View::Graph => vec![
            Span::styled(" q", theme.status_bar_keys),
            Span::styled(" quit  ", theme.status_bar),
            Span::styled("j/k", theme.status_bar_keys),
            Span::styled(" nav  ", theme.status_bar),
            Span::styled("Tab", theme.status_bar_keys),
            Span::styled(" mode  ", theme.status_bar),
            Span::styled("Enter", theme.status_bar_keys),
            Span::styled(" open  ", theme.status_bar),
            Span::styled("?", theme.status_bar_keys),
            Span::styled(" help", theme.status_bar),
        ],
    }
}
