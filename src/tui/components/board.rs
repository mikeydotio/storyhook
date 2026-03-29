use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::domain::{Priority, SuperState};
use crate::tui::action::Action;
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::{Component, HitRegion};

/// An item in the flat visible-rows list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowItem {
    SectionHeader {
        slug: String,
        name: String,
        count: usize,
        expanded: bool,
    },
    StoryRow {
        id: String,
    },
}

/// The board component: stories grouped by state as a scrollable list.
pub struct Board {
    pub cursor: usize,
    pub collapsed: HashSet<String>,
    pub hit_regions: Vec<HitRegion>,
    pub scroll_offset: usize,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl Board {
    pub fn new() -> Self {
        Self {
            cursor: 0,
            collapsed: HashSet::new(),
            hit_regions: Vec::new(),
            scroll_offset: 0,
        }
    }

    /// Build the flat list of visible rows, respecting collapsed sections.
    pub fn build_visible_rows(&self, state: &AppState) -> Vec<RowItem> {
        let groups = state.data.stories_by_state();
        let mut rows = Vec::new();

        for (state_def, stories) in &groups {
            let expanded = !self.collapsed.contains(&state_def.slug);
            rows.push(RowItem::SectionHeader {
                slug: state_def.slug.clone(),
                name: state_def.slug.clone(),
                count: stories.len(),
                expanded,
            });
            if expanded {
                for story in stories {
                    rows.push(RowItem::StoryRow {
                        id: story.id.clone(),
                    });
                }
            }
        }

        rows
    }

    /// Clamp cursor to valid range for the given visible rows.
    fn clamp_cursor(&mut self, visible_len: usize) {
        if visible_len == 0 {
            self.cursor = 0;
        } else if self.cursor >= visible_len {
            self.cursor = visible_len - 1;
        }
    }

    /// Update scroll_offset so the cursor is visible in a viewport of the given height.
    fn update_scroll(&mut self, viewport_height: usize) {
        if viewport_height == 0 {
            return;
        }
        if self.cursor < self.scroll_offset {
            self.scroll_offset = self.cursor;
        } else if self.cursor >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.cursor - viewport_height + 1;
        }
    }

    /// Find the index of the next section header at or after `from`.
    fn next_section_header(&self, rows: &[RowItem], from: usize) -> Option<usize> {
        rows.iter()
            .enumerate()
            .skip(from + 1)
            .find(|(_, r)| matches!(r, RowItem::SectionHeader { .. }))
            .map(|(i, _)| i)
    }

    /// Find the index of the previous section header before `from`.
    fn prev_section_header(&self, rows: &[RowItem], from: usize) -> Option<usize> {
        if from == 0 {
            return None;
        }
        rows.iter()
            .enumerate()
            .take(from)
            .rev()
            .find(|(_, r)| matches!(r, RowItem::SectionHeader { .. }))
            .map(|(i, _)| i)
    }

    /// Determine the next state for a story (for > / L key).
    /// Returns None if the story is already in the last OPEN state.
    fn next_state_for_story(&self, story_id: &str, state: &AppState) -> Option<String> {
        let story = state.data.find_story(story_id)?;
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
            // Already at last OPEN state, check if there's a CLOSED state
            state
                .data
                .states
                .iter()
                .find(|s| s.super_state == SuperState::Closed)
                .map(|s| s.slug.clone())
        }
    }

    /// Determine the previous state for a story (for < / H key).
    /// Returns None if the story is already in the first state.
    fn prev_state_for_story(&self, story_id: &str, state: &AppState) -> Option<String> {
        let story = state.data.find_story(story_id)?;
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
}

impl Component for Board {
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        let rows = self.build_visible_rows(state);
        if rows.is_empty() {
            // Only 'n' and '/' work on empty board
            if key.code == KeyCode::Char('n') {
                return vec![Action::OpenCreateForm];
            }
            if key.code == KeyCode::Char('/') {
                return vec![Action::FocusFilterBar];
            }
            return vec![];
        }

        // Content area height: terminal height minus 1 for status bar
        let viewport_height = state.terminal_size.1.saturating_sub(1) as usize;

        let result = match key.code {
            // Cursor movement
            KeyCode::Char('j') | KeyCode::Down => {
                if self.cursor + 1 < rows.len() {
                    self.cursor += 1;
                }
                vec![]
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                vec![]
            }

            // Jump to section headers
            KeyCode::Char('h') if key.modifiers == KeyModifiers::NONE => {
                if let Some(idx) = self.prev_section_header(&rows, self.cursor) {
                    self.cursor = idx;
                }
                vec![]
            }
            KeyCode::Char('l') if key.modifiers == KeyModifiers::NONE => {
                if let Some(idx) = self.next_section_header(&rows, self.cursor) {
                    self.cursor = idx;
                }
                vec![]
            }

            // First/Last row
            KeyCode::Char('g') if key.modifiers == KeyModifiers::NONE => {
                self.cursor = 0;
                vec![]
            }
            KeyCode::Char('G') if key.modifiers == KeyModifiers::SHIFT => {
                self.cursor = rows.len().saturating_sub(1);
                vec![]
            }

            // Toggle section collapse
            KeyCode::Char(' ') => {
                if let Some(RowItem::SectionHeader { slug, .. }) = rows.get(self.cursor) {
                    if self.collapsed.contains(slug) {
                        self.collapsed.remove(slug);
                    } else {
                        self.collapsed.insert(slug.clone());
                    }
                    // Reclamp cursor after collapse
                    let new_rows = self.build_visible_rows(state);
                    self.clamp_cursor(new_rows.len());
                }
                vec![]
            }

            // Open story detail
            KeyCode::Enter => {
                if let Some(RowItem::StoryRow { id }) = rows.get(self.cursor) {
                    vec![Action::OpenDetail(id.clone())]
                } else {
                    vec![]
                }
            }

            // Move story forward (next state)
            KeyCode::Char('>') | KeyCode::Char('L') => {
                if let Some(RowItem::StoryRow { id }) = rows.get(self.cursor) {
                    if let Some(target) = self.next_state_for_story(id, state) {
                        vec![Action::MoveStory {
                            id: id.clone(),
                            target_state: target,
                        }]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            }

            // Move story backward (previous state)
            KeyCode::Char('<') | KeyCode::Char('H') => {
                if let Some(RowItem::StoryRow { id }) = rows.get(self.cursor) {
                    if let Some(target) = self.prev_state_for_story(id, state) {
                        vec![Action::MoveStory {
                            id: id.clone(),
                            target_state: target,
                        }]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            }

            // Create new story
            KeyCode::Char('n') => vec![Action::OpenCreateForm],

            // Focus filter bar
            KeyCode::Char('/') => vec![Action::FocusFilterBar],

            _ => vec![],
        };

        // Keep scroll in sync with cursor
        self.update_scroll(viewport_height);

        result
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent, _state: &AppState) -> Vec<Action> {
        // Mouse handling deferred to later wave
        vec![]
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let theme = Theme::from_env();
        let rows = self.build_visible_rows(state);

        if rows.is_empty() {
            let empty_msg = Line::from(Span::styled(
                "No stories yet. Press n to create one.",
                theme.section_count,
            ));
            frame.render_widget(
                Paragraph::new(empty_msg)
                    .alignment(ratatui::layout::Alignment::Center),
                area,
            );
            return;
        }

        let viewport_height = area.height as usize;
        // scroll_offset is kept in sync by handle_key and on_state_change.
        // Compute a local adjustment in case the viewport size changed since
        // the last key event (e.g., terminal resize).
        let scroll_offset = if viewport_height > 0 && self.cursor >= self.scroll_offset + viewport_height {
            self.cursor - viewport_height + 1
        } else if self.cursor < self.scroll_offset {
            self.cursor
        } else {
            self.scroll_offset
        };

        let width = area.width as usize;
        let mut lines: Vec<Line> = Vec::with_capacity(viewport_height);

        for (i, row) in rows
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(viewport_height)
        {
            let is_selected = i == self.cursor;

            match row {
                RowItem::SectionHeader {
                    name,
                    count,
                    expanded,
                    ..
                } => {
                    lines.push(render_section_header(
                        name,
                        *count,
                        *expanded,
                        is_selected,
                        width,
                        &theme,
                    ));
                }
                RowItem::StoryRow { id } => {
                    let story = state.data.find_story(id);
                    lines.push(render_story_row(story, is_selected, width, &theme));
                }
            }
        }

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn on_state_change(&mut self, state: &AppState) {
        let rows = self.build_visible_rows(state);
        self.clamp_cursor(rows.len());
        let viewport_height = state.terminal_size.1.saturating_sub(1) as usize;
        self.update_scroll(viewport_height);
    }

    fn hit_regions(&self) -> &[HitRegion] {
        &self.hit_regions
    }
}

/// Render a section header line.
fn render_section_header(
    name: &str,
    count: usize,
    expanded: bool,
    is_selected: bool,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let triangle = if expanded { "\u{25BC}" } else { "\u{25B6}" };
    let prefix = format!("{triangle} {name} ({count}) ");
    let prefix_len = prefix.chars().count();

    // Fill remaining width with horizontal line
    let fill_len = width.saturating_sub(prefix_len);
    let fill: String = "\u{2500}".repeat(fill_len);

    let mut spans = vec![
        Span::styled(
            format!("{triangle} "),
            theme.disclosure,
        ),
        Span::styled(
            name.to_string(),
            if is_selected {
                theme.section_header.patch(theme.selected_row)
            } else {
                theme.section_header
            },
        ),
        Span::styled(format!(" ({count}) "), theme.section_count),
        Span::styled(fill, theme.section_count),
    ];

    if is_selected {
        // Wrap the entire line with selected_row background
        for span in &mut spans {
            span.style = span.style.patch(theme.selected_row);
        }
    }

    Line::from(spans)
}

/// Render a story row line.
fn render_story_row(
    story: Option<&crate::domain::StorySnapshot>,
    is_selected: bool,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let Some(story) = story else {
        return Line::from(Span::raw("  (unknown story)"));
    };

    let cursor_mark = if is_selected { "> " } else { "  " };

    // Build right-side badges first so we know how much space is left for title
    let mut right_parts: Vec<(String, ratatui::style::Style)> = Vec::new();

    // Priority symbol
    let (pri_sym, pri_style) = priority_display(&story.priority, theme);
    if !pri_sym.is_empty() {
        right_parts.push((format!(" {pri_sym}"), pri_style));
    }

    // Labels
    for label in &story.labels {
        right_parts.push((format!(" [{label}]"), theme.labels));
    }

    // Assignee
    if let Some(ref assignee) = story.assignee {
        right_parts.push((format!(" @{assignee}"), theme.assignee));
    }

    // Blocked badge
    if story.awaiting.is_some() {
        right_parts.push((" BLK".to_string(), theme.blocked_badge));
    }

    // Calculate right-side width
    let right_width: usize = right_parts.iter().map(|(s, _)| s.chars().count()).sum();

    // ID display (e.g. "SH-12")
    let id_display = format!("{:<8}", &story.id);
    let id_width = id_display.chars().count();

    // cursor(2) + id(8) + gap(1) + title + right
    let fixed_width = 2 + id_width + 1 + right_width;
    let title_budget = width.saturating_sub(fixed_width);

    let title = if story.title.chars().count() > title_budget {
        let truncated: String = story.title.chars().take(title_budget.saturating_sub(1)).collect();
        format!("{truncated}\u{2026}")
    } else {
        // Pad title to fill available space
        let pad = title_budget.saturating_sub(story.title.chars().count());
        format!("{}{}", story.title, " ".repeat(pad))
    };

    let mut spans = Vec::new();

    // Cursor marker
    spans.push(Span::styled(
        cursor_mark.to_string(),
        if is_selected { theme.cursor } else { theme.story_id },
    ));

    // Story ID
    spans.push(Span::styled(id_display, theme.story_id));

    // Gap
    spans.push(Span::raw(" "));

    // Title
    spans.push(Span::styled(title, theme.story_title));

    // Right-side badges
    for (text, style) in right_parts {
        spans.push(Span::styled(text, style));
    }

    if is_selected {
        for span in &mut spans {
            span.style = span.style.patch(theme.selected_row);
        }
    }

    Line::from(spans)
}

/// Get the priority display symbol and style.
fn priority_display(priority: &Priority, theme: &Theme) -> (&'static str, ratatui::style::Style) {
    match priority {
        Priority::Critical => ("!!!", theme.priority_critical),
        Priority::High => ("!!", theme.priority_high),
        Priority::Medium => ("!", theme.priority_medium),
        Priority::Low => (".", theme.priority_low),
        Priority::None => ("", theme.status_bar), // invisible, no symbol
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Priority, StateDef, StorySnapshot, SuperState};
    use crate::tui::action::View;
    use crate::tui::data::DataStore;
    use crate::tui::focus::{FocusStack, FocusTarget};
    use crate::tui::state::AppState;

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
                slug: "review".to_string(),
                super_state: SuperState::Open,
                role: None,
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
            },
        ]
    }

    fn test_snapshot(id: &str, state: &str, title: &str) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: title.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: state.to_string(),
            superstate: if state == "done" {
                SuperState::Closed
            } else {
                SuperState::Open
            },
            assignee: None,
            awaiting: None,
            comments: vec![],
            relationships: vec![],
            priority: Priority::None,
            labels: vec![],
            closed_at: None,
        }
    }

    fn make_state(data: DataStore) -> AppState {
        AppState {
            data,
            focus: FocusStack::new(FocusTarget::Board),
            view: View::Board,
            filters: Vec::new(),
            filter_bar_focused: false,
            running: true,
            notification: None,
            terminal_size: (80, 24),
        }
    }

    // --- Task 2.1 Tests: Visible Rows + Collapse ---

    #[test]
    fn visible_rows_all_expanded() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
                test_snapshot("SH-2", "todo", "Second"),
                test_snapshot("SH-3", "in-progress", "Third"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let board = Board::new();
        let rows = board.build_visible_rows(&state);

        // 3 open state headers (todo, in-progress, review) + 3 story rows
        assert_eq!(rows.len(), 6);
        assert!(matches!(&rows[0], RowItem::SectionHeader { slug, count: 2, expanded: true, .. } if slug == "todo"));
        assert!(matches!(&rows[1], RowItem::StoryRow { id } if id == "SH-1"));
        assert!(matches!(&rows[2], RowItem::StoryRow { id } if id == "SH-2"));
        assert!(matches!(&rows[3], RowItem::SectionHeader { slug, count: 1, expanded: true, .. } if slug == "in-progress"));
        assert!(matches!(&rows[4], RowItem::StoryRow { id } if id == "SH-3"));
        assert!(matches!(&rows[5], RowItem::SectionHeader { slug, count: 0, expanded: true, .. } if slug == "review"));
    }

    #[test]
    fn visible_rows_with_collapsed_section() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
                test_snapshot("SH-2", "todo", "Second"),
                test_snapshot("SH-3", "in-progress", "Third"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.collapsed.insert("todo".to_string());
        let rows = board.build_visible_rows(&state);

        // todo header (collapsed, no stories) + in-progress header + 1 story + review header
        assert_eq!(rows.len(), 4);
        assert!(matches!(&rows[0], RowItem::SectionHeader { slug, count: 2, expanded: false, .. } if slug == "todo"));
        assert!(matches!(&rows[1], RowItem::SectionHeader { slug, count: 1, expanded: true, .. } if slug == "in-progress"));
        assert!(matches!(&rows[2], RowItem::StoryRow { id } if id == "SH-3"));
        assert!(matches!(&rows[3], RowItem::SectionHeader { slug, count: 0, expanded: true, .. } if slug == "review"));
    }

    #[test]
    fn visible_rows_empty_project() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let board = Board::new();
        let rows = board.build_visible_rows(&state);

        // Only section headers (3 open states), no story rows
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert!(matches!(row, RowItem::SectionHeader { count: 0, .. }));
        }
    }

    #[test]
    fn cursor_clamps_after_collapse() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
                test_snapshot("SH-2", "todo", "Second"),
                test_snapshot("SH-3", "in-progress", "Third"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();

        // Place cursor on second story row (index 2)
        board.cursor = 2;

        // Collapse todo: removes 2 story rows, so rows shrink from 6 to 4
        board.collapsed.insert("todo".to_string());
        let rows = board.build_visible_rows(&state);
        board.clamp_cursor(rows.len());

        // Cursor 2 is still valid (in-progress story)
        assert_eq!(board.cursor, 2);

        // Now place cursor at the end
        board.cursor = 5; // Was valid before collapse
        board.clamp_cursor(rows.len()); // 4 rows, so clamped to 3
        assert_eq!(board.cursor, 3);
    }

    // --- Task 2.3 Tests: Keyboard Navigation ---

    #[test]
    fn cursor_movement_j_k() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
                test_snapshot("SH-2", "in-progress", "Second"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        assert_eq!(board.cursor, 0);

        // j moves down
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 1);

        // j again
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 2);

        // k moves up
        board.handle_key(
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 1);

        // k at 0 stays at 0
        board.cursor = 0;
        board.handle_key(
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 0);
    }

    #[test]
    fn cursor_bounds_at_last_row() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();

        // Move to last row
        let rows = board.build_visible_rows(&state);
        board.cursor = rows.len() - 1;
        let last = board.cursor;

        // j at last row stays put
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, last);
    }

    #[test]
    fn jump_to_section_headers() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
                test_snapshot("SH-2", "todo", "Second"),
                test_snapshot("SH-3", "in-progress", "Third"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.cursor = 0; // at "todo" header

        // l jumps to next section header (in-progress at index 3)
        board.handle_key(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 3);

        // l again jumps to review (index 5)
        board.handle_key(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 5);

        // h jumps back to in-progress (index 3)
        board.handle_key(
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 3);

        // h again jumps to todo (index 0)
        board.handle_key(
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 0);
    }

    #[test]
    fn space_toggles_section_collapse() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
                test_snapshot("SH-2", "todo", "Second"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.cursor = 0; // on "todo" header

        // Initially expanded
        assert!(!board.collapsed.contains("todo"));

        // Space collapses
        board.handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &state,
        );
        assert!(board.collapsed.contains("todo"));

        // Space again expands
        board.handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &state,
        );
        assert!(!board.collapsed.contains("todo"));
    }

    #[test]
    fn enter_on_story_row_emits_open_detail() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.cursor = 1; // on SH-1 story row

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &state,
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::OpenDetail(id) if id == "SH-1"));
    }

    #[test]
    fn enter_on_header_does_nothing() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.cursor = 0; // on header

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &state,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn move_story_forward() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.cursor = 1; // on SH-1

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT),
            &state,
        );
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::MoveStory { id, target_state } if id == "SH-1" && target_state == "in-progress")
        );
    }

    #[test]
    fn move_story_backward() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "in-progress", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        // Rows: [0] todo hdr, [1] in-progress hdr, [2] SH-1, [3] review hdr
        board.cursor = 2; // on SH-1

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT),
            &state,
        );
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::MoveStory { id, target_state } if id == "SH-1" && target_state == "todo")
        );
    }

    #[test]
    fn move_story_backward_at_first_state_does_nothing() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.cursor = 1; // on SH-1

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT),
            &state,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn move_story_forward_at_last_open_goes_to_closed() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "review", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        // Rows: [0] todo hdr, [1] in-progress hdr, [2] review hdr, [3] SH-1
        board.cursor = 3; // on SH-1 (in review section, which is third open state)

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT),
            &state,
        );
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::MoveStory { id, target_state } if id == "SH-1" && target_state == "done")
        );
    }

    #[test]
    fn g_and_shift_g_jump_to_first_and_last() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
                test_snapshot("SH-2", "in-progress", "Second"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.cursor = 3;

        // g goes to first
        board.handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 0);

        // G goes to last
        board.handle_key(
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
            &state,
        );
        let rows = board.build_visible_rows(&state);
        assert_eq!(board.cursor, rows.len() - 1);
    }
}
