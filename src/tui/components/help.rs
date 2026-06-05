use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::action::Action;
use crate::tui::components::modal::render_modal;
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::Component;

/// Help overlay component: full keybinding reference organized by context.
pub struct Help {
    pub scroll_offset: u16,
}

impl Default for Help {
    fn default() -> Self {
        Self::new()
    }
}

impl Help {
    pub fn new() -> Self {
        Self { scroll_offset: 0 }
    }
}

impl Component for Help {
    fn handle_key(&mut self, key: KeyEvent, _state: &AppState) -> Vec<Action> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                vec![]
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                vec![]
            }
            // Esc/q/? are handled by the keymap layer, which emits CloseModal.
            // If they somehow reach here, close the modal.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                vec![Action::CloseModal]
            }
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent, _state: &AppState) -> Vec<Action> {
        vec![]
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _state: &AppState) {
        let theme = Theme::from_env();
        let inner = render_modal(frame, area, "Help", &theme);

        let lines = build_help_lines(&theme);
        let total = lines.len();
        let visible_height = inner.height as usize;

        // Clamp scroll offset
        let max_scroll = total.saturating_sub(visible_height);
        let scroll = (self.scroll_offset as usize).min(max_scroll);

        let visible: Vec<Line> = lines
            .into_iter()
            .skip(scroll)
            .take(visible_height)
            .collect();
        frame.render_widget(Paragraph::new(visible), inner);
    }
}

/// Build all help content lines, organized by context.
fn build_help_lines(theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Global
    section_header(&mut lines, "Global", theme);
    key_row(&mut lines, "q", "Quit", theme);
    key_row(&mut lines, "Ctrl+C", "Force quit", theme);
    key_row(&mut lines, "?", "Toggle help overlay", theme);
    key_row(&mut lines, "1", "Switch to Dashboard", theme);
    key_row(&mut lines, "2", "Switch to Board", theme);
    key_row(&mut lines, "3", "Switch to Graph", theme);
    key_row(&mut lines, "r", "Refresh data from disk", theme);
    key_row(&mut lines, "Ctrl+Z", "Undo last action", theme);
    key_row(&mut lines, "Ctrl+Y", "Redo last undone action", theme);
    lines.push(Line::from(""));

    // Board
    section_header(&mut lines, "Board View", theme);
    key_row(&mut lines, "j / Down", "Next row", theme);
    key_row(&mut lines, "k / Up", "Previous row", theme);
    key_row(&mut lines, "h", "Jump to previous section header", theme);
    key_row(&mut lines, "l", "Jump to next section header", theme);
    key_row(&mut lines, "g", "Jump to first row", theme);
    key_row(&mut lines, "G", "Jump to last row", theme);
    key_row(
        &mut lines,
        "Space",
        "Toggle section collapsed/expanded",
        theme,
    );
    key_row(&mut lines, "Enter", "Open story detail", theme);
    key_row(&mut lines, "> / L", "Move story to next state", theme);
    key_row(&mut lines, "< / H", "Move story to previous state", theme);
    key_row(&mut lines, "n", "Open create form", theme);
    key_row(&mut lines, "/", "Focus filter bar", theme);
    lines.push(Line::from(""));

    // Filter Bar
    section_header(&mut lines, "Filter Bar (focused)", theme);
    key_row(&mut lines, "Text input", "Type filter query", theme);
    key_row(&mut lines, "Enter", "Apply filter", theme);
    key_row(&mut lines, "Tab", "Accept suggestion", theme);
    key_row(
        &mut lines,
        "Backspace",
        "Remove last chip (when empty)",
        theme,
    );
    key_row(&mut lines, "Esc", "Unfocus, return to board", theme);
    key_row(&mut lines, "Ctrl+U", "Clear all filters", theme);
    lines.push(Line::from(""));

    // Story Detail
    section_header(&mut lines, "Story Detail Modal", theme);
    key_row(&mut lines, "Esc", "Close modal (or cancel edit)", theme);
    key_row(&mut lines, "j / Down", "Next field", theme);
    key_row(&mut lines, "k / Up", "Previous field", theme);
    key_row(&mut lines, "e", "Edit selected field", theme);
    key_row(&mut lines, "Enter", "Confirm edit", theme);
    key_row(&mut lines, "c", "Add comment", theme);
    key_row(&mut lines, "Ctrl+E", "Open $EDITOR", theme);
    key_row(&mut lines, ">", "Move to next state", theme);
    key_row(&mut lines, "<", "Move to previous state", theme);
    key_row(
        &mut lines,
        "Tab / Shift+Tab",
        "Cycle fields (during edit)",
        theme,
    );
    lines.push(Line::from(""));

    // Create Form
    section_header(&mut lines, "Create Form Modal", theme);
    key_row(&mut lines, "Esc", "Cancel and close", theme);
    key_row(&mut lines, "Tab / Shift+Tab", "Cycle fields", theme);
    key_row(&mut lines, "Enter", "Submit (if title non-empty)", theme);
    lines.push(Line::from(""));

    // Graph View
    section_header(&mut lines, "Graph View", theme);
    key_row(&mut lines, "j / Down", "Next row", theme);
    key_row(&mut lines, "k / Up", "Previous row", theme);
    key_row(&mut lines, "g", "Jump to first row", theme);
    key_row(&mut lines, "G", "Jump to last row", theme);
    key_row(
        &mut lines,
        "Tab / m",
        "Cycle modes (Tree/Deps/Critical/Focus)",
        theme,
    );
    key_row(&mut lines, "Enter", "Open detail or set focus story", theme);
    key_row(&mut lines, "Esc", "Exit focus mode", theme);
    key_row(&mut lines, "n", "Open create form", theme);
    lines.push(Line::from(""));

    // Help Overlay
    section_header(&mut lines, "Help Overlay", theme);
    key_row(&mut lines, "Esc / q / ?", "Close", theme);
    key_row(&mut lines, "j / k", "Scroll", theme);

    lines
}

/// Add a section header line.
fn section_header(lines: &mut Vec<Line<'static>>, title: &str, theme: &Theme) {
    lines.push(Line::from(Span::styled(
        format!(" {title}"),
        theme.section_header,
    )));
    lines.push(Line::from(Span::styled(
        " ".to_string() + &"\u{2500}".repeat(40),
        theme.section_count,
    )));
}

/// Add a two-column key/action row.
fn key_row(lines: &mut Vec<Line<'static>>, key: &str, action: &str, theme: &Theme) {
    let key_col_width = 22;
    let padded_key = format!("  {:<width$}", key, width = key_col_width);
    lines.push(Line::from(vec![
        Span::styled(padded_key, theme.status_bar_keys),
        Span::styled(action.to_string(), theme.status_bar),
    ]));
}

/// Returns all keys that appear in the help overlay, for test verification.
pub fn all_help_keys() -> Vec<&'static str> {
    vec![
        // Global
        "q",
        "Ctrl+C",
        "?",
        "1",
        "2",
        "3",
        "r",
        "Ctrl+Z",
        "Ctrl+Y",
        // Board
        "j / Down",
        "k / Up",
        "h",
        "l",
        "g",
        "G",
        "Space",
        "Enter",
        "> / L",
        "< / H",
        "n",
        "/",
        // Filter Bar
        "Text input",
        "Enter",
        "Tab",
        "Backspace",
        "Esc",
        "Ctrl+U",
        // Story Detail
        "Esc",
        "j / Down",
        "k / Up",
        "e",
        "Enter",
        "c",
        "Ctrl+E",
        ">",
        "<",
        "Tab / Shift+Tab",
        // Create Form
        "Esc",
        "Tab / Shift+Tab",
        "Enter",
        // Graph View
        "Tab / m",
        // Help
        "Esc / q / ?",
        "j / k",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Theme;

    #[test]
    fn help_contains_all_design_keys() {
        // Every keybinding from DESIGN.md should appear in the help lines.
        // We check that key strings from all_help_keys() appear in the rendered text.
        let theme = Theme::colored();
        let lines = build_help_lines(&theme);
        let full_text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Check all design-specified keys
        let design_keys = vec![
            "q",
            "Ctrl+C",
            "?",
            "1",
            "2",
            "3",
            "r",
            "Ctrl+Z",
            "Ctrl+Y",
            "j / Down",
            "k / Up",
            "h",
            "l",
            "g",
            "G",
            "Space",
            "Enter",
            "> / L",
            "< / H",
            "n",
            "/",
            "Tab",
            "Backspace",
            "Esc",
            "Ctrl+U",
            "e",
            "c",
            "Ctrl+E",
            ">",
            "<",
            "Tab / Shift+Tab",
            "Tab / m",
            "Esc / q / ?",
            "j / k",
        ];

        for key in &design_keys {
            assert!(
                full_text.contains(key),
                "Help overlay is missing key: {key}"
            );
        }
    }

    #[test]
    fn help_has_all_sections() {
        let theme = Theme::colored();
        let lines = build_help_lines(&theme);
        let full_text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(full_text.contains("Global"), "Missing Global section");
        assert!(
            full_text.contains("Board View"),
            "Missing Board View section"
        );
        assert!(
            full_text.contains("Filter Bar"),
            "Missing Filter Bar section"
        );
        assert!(
            full_text.contains("Story Detail"),
            "Missing Story Detail section"
        );
        assert!(
            full_text.contains("Create Form"),
            "Missing Create Form section"
        );
        assert!(
            full_text.contains("Graph View"),
            "Missing Graph View section"
        );
        assert!(
            full_text.contains("Help Overlay"),
            "Missing Help Overlay section"
        );
    }

    #[test]
    fn help_scroll_j_k() {
        let mut help = Help::new();
        let state = test_state();
        assert_eq!(help.scroll_offset, 0);

        // j scrolls down
        help.handle_key(
            crossterm::event::KeyEvent::new(
                KeyCode::Char('j'),
                crossterm::event::KeyModifiers::NONE,
            ),
            &state,
        );
        assert_eq!(help.scroll_offset, 1);

        // k scrolls up
        help.handle_key(
            crossterm::event::KeyEvent::new(
                KeyCode::Char('k'),
                crossterm::event::KeyModifiers::NONE,
            ),
            &state,
        );
        assert_eq!(help.scroll_offset, 0);

        // k at 0 stays at 0 (saturating)
        help.handle_key(
            crossterm::event::KeyEvent::new(
                KeyCode::Char('k'),
                crossterm::event::KeyModifiers::NONE,
            ),
            &state,
        );
        assert_eq!(help.scroll_offset, 0);
    }

    fn test_state() -> AppState {
        use crate::tui::action::View;
        use crate::tui::data::DataStore;
        use crate::tui::focus::{FocusStack, FocusTarget};
        AppState {
            data: DataStore::from_test_data(vec![], vec![], "SH".to_string(), vec![]),
            focus: FocusStack::new(FocusTarget::Board),
            view: View::Board,
            filters: Vec::new(),
            filter_bar_focused: false,
            running: true,
            notification: None,
            terminal_size: (80, 24),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }
}
