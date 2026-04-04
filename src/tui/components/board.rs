use std::collections::HashSet;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::domain::{Priority, SuperState};
use crate::tui::action::Action;
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::{Component, HitRegion, HitTarget};

/// Tracks the state of a drag-and-drop operation on the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragState {
    Idle,
    Pending {
        story_id: String,
        origin_row: usize,
        start_col: u16,
        start_row: u16,
    },
    Dragging {
        story_id: String,
        origin_row: usize,
        current_target_section: Option<String>,
    },
}

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
    /// Last click time and row index for double-click detection.
    last_click: Option<(Instant, usize)>,
    /// The render area from the last render, used to interpret mouse coordinates.
    render_area: Rect,
    /// Current drag-and-drop state.
    pub drag_state: DragState,
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
            last_click: None,
            render_area: Rect::default(),
            drag_state: DragState::Idle,
        }
    }

    /// Build the flat list of visible rows, respecting collapsed sections and active filters.
    pub fn build_visible_rows(&self, state: &AppState) -> Vec<RowItem> {
        let groups = state.data.filtered_stories_by_state(&state.filters);
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

    /// Find the section header slug at the given screen row, using hit_regions.
    fn find_section_at(&self, row: u16) -> Option<String> {
        self.hit_regions.iter().find_map(|region| {
            if row >= region.rect.y
                && row < region.rect.y + region.rect.height
                && let HitTarget::SectionHeader { state_slug } = &region.target
            {
                return Some(state_slug.clone());
            }
            None
        })
    }
}

impl Component for Board {
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        // Cancel any active drag on Esc
        if self.drag_state != DragState::Idle && key.code == KeyCode::Esc {
            self.drag_state = DragState::Idle;
            return vec![];
        }

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

        // Content area height: terminal height minus 1 for status bar minus 1 for filter bar
        let viewport_height = state.terminal_size.1.saturating_sub(2) as usize;

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

    fn handle_mouse(&mut self, mouse: MouseEvent, state: &AppState) -> Vec<Action> {
        let rows = self.build_visible_rows(state);
        if rows.is_empty() {
            return vec![];
        }
        let viewport_height = self.render_area.height as usize;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Determine which row was clicked based on mouse Y position
                // relative to the render area
                if mouse.column < self.render_area.x
                    || mouse.column >= self.render_area.x + self.render_area.width
                    || mouse.row < self.render_area.y
                    || mouse.row >= self.render_area.y + self.render_area.height
                {
                    return vec![];
                }

                let click_y = (mouse.row - self.render_area.y) as usize;
                let row_index = self.scroll_offset + click_y;

                if row_index >= rows.len() {
                    return vec![];
                }

                // Double-click detection: same row within 300ms
                let now = Instant::now();
                let is_double_click = if let Some((last_time, last_row)) = self.last_click {
                    last_row == row_index
                        && now.duration_since(last_time).as_millis() < 300
                } else {
                    false
                };

                self.last_click = Some((now, row_index));
                self.cursor = row_index;
                self.update_scroll(viewport_height);

                if is_double_click {
                    // Double-click on story row opens detail
                    if let Some(RowItem::StoryRow { id }) = rows.get(row_index) {
                        return vec![Action::OpenDetail(id.clone())];
                    }
                }

                // Start a pending drag if clicking on a story row
                if let Some(RowItem::StoryRow { id }) = rows.get(row_index) {
                    self.drag_state = DragState::Pending {
                        story_id: id.clone(),
                        origin_row: row_index,
                        start_col: mouse.column,
                        start_row: mouse.row,
                    };
                }

                vec![]
            }

            MouseEventKind::Drag(MouseButton::Left) => {
                match &self.drag_state {
                    DragState::Pending {
                        story_id,
                        origin_row,
                        start_col,
                        start_row,
                    } => {
                        // Manhattan distance threshold to avoid accidental drags
                        let dx = mouse.column.abs_diff(*start_col);
                        let dy = mouse.row.abs_diff(*start_row);
                        if dx + dy > 3 {
                            // Determine if the mouse is currently over a section header
                            let target_section = self.find_section_at(mouse.row);
                            self.drag_state = DragState::Dragging {
                                story_id: story_id.clone(),
                                origin_row: *origin_row,
                                current_target_section: target_section,
                            };
                        }
                        vec![]
                    }
                    DragState::Dragging {
                        story_id,
                        origin_row,
                        ..
                    } => {
                        // Update the target section as the mouse moves
                        let target_section = self.find_section_at(mouse.row);
                        self.drag_state = DragState::Dragging {
                            story_id: story_id.clone(),
                            origin_row: *origin_row,
                            current_target_section: target_section,
                        };
                        vec![]
                    }
                    DragState::Idle => vec![],
                }
            }

            MouseEventKind::Up(MouseButton::Left) => {
                match std::mem::replace(&mut self.drag_state, DragState::Idle) {
                    DragState::Dragging {
                        story_id,
                        current_target_section: Some(target_slug),
                        ..
                    } => {
                        // Successful drop on a section header
                        vec![Action::MoveStory {
                            id: story_id,
                            target_state: target_slug,
                        }]
                    }
                    DragState::Dragging { .. } => {
                        // Dropped outside any valid section -- cancel
                        vec![]
                    }
                    DragState::Pending { .. } => {
                        // No drag movement occurred -- treat as normal click
                        // (last_click and cursor were already set in MouseDown)
                        vec![]
                    }
                    DragState::Idle => vec![],
                }
            }

            MouseEventKind::ScrollUp => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.update_scroll(viewport_height);
                }
                vec![]
            }
            MouseEventKind::ScrollDown => {
                if self.cursor + 1 < rows.len() {
                    self.cursor += 1;
                    self.update_scroll(viewport_height);
                }
                vec![]
            }
            _ => vec![],
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        self.render_area = area;

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
            self.hit_regions.clear();
            return;
        }

        // Check if filters are active but produced no story rows
        let has_story_rows = rows.iter().any(|r| matches!(r, RowItem::StoryRow { .. }));
        if !has_story_rows && !state.filters.is_empty() {
            let empty_msg = Line::from(Span::styled(
                "No stories match current filters.",
                theme.section_count,
            ));
            frame.render_widget(
                Paragraph::new(empty_msg)
                    .alignment(ratatui::layout::Alignment::Center),
                area,
            );
            self.hit_regions.clear();
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

        // Extract drag info for visual feedback
        let (drag_source_id, drag_target_slug) = match &self.drag_state {
            DragState::Dragging {
                story_id,
                current_target_section,
                ..
            } => (Some(story_id.clone()), current_target_section.clone()),
            _ => (None, None),
        };

        // Populate hit_regions for mouse support
        self.hit_regions.clear();

        for (vi, (i, row)) in rows
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(viewport_height)
            .enumerate()
        {
            let is_selected = i == self.cursor;

            // Register hit region for this visible row
            let row_rect = Rect::new(area.x, area.y + vi as u16, area.width, 1);
            let target = match row {
                RowItem::SectionHeader { slug, .. } => {
                    HitTarget::SectionHeader { state_slug: slug.clone() }
                }
                RowItem::StoryRow { id } => {
                    HitTarget::StoryRow { id: id.clone() }
                }
            };
            self.hit_regions.push(HitRegion { rect: row_rect, target });

            match row {
                RowItem::SectionHeader {
                    slug,
                    name,
                    count,
                    expanded,
                } => {
                    let is_drag_target = drag_target_slug.as_deref() == Some(slug);
                    let mut line = render_section_header(
                        name,
                        *count,
                        *expanded,
                        is_selected,
                        width,
                        &theme,
                    );
                    if is_drag_target {
                        for span in &mut line.spans {
                            span.style = theme.drag_target;
                        }
                    }
                    lines.push(line);
                }
                RowItem::StoryRow { id } => {
                    let is_drag_source = drag_source_id.as_deref() == Some(id.as_str());
                    let story = state.data.find_story(id);
                    let mut line = render_story_row(story, is_selected, width, &theme);
                    if is_drag_source {
                        for span in &mut line.spans {
                            span.style = span.style.patch(theme.drag_source);
                        }
                    }
                    lines.push(line);
                }
            }
        }

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn on_state_change(&mut self, state: &AppState) {
        let rows = self.build_visible_rows(state);
        self.clamp_cursor(rows.len());
        let viewport_height = state.terminal_size.1.saturating_sub(2) as usize;
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

    // Assignee (compute first to know fixed overhead)
    let assignee_part: Option<(String, ratatui::style::Style)> =
        story.assignee.as_ref().map(|a| (format!(" @{a}"), theme.assignee));
    let blocked_part: Option<(String, ratatui::style::Style)> = if story.awaiting.is_some() {
        Some((" BLK".to_string(), theme.blocked_badge))
    } else {
        None
    };

    // Calculate non-label right-side width to figure out label budget
    let non_label_right: usize = right_parts.iter().map(|(s, _)| s.chars().count()).sum::<usize>()
        + assignee_part.as_ref().map_or(0, |(s, _)| s.chars().count())
        + blocked_part.as_ref().map_or(0, |(s, _)| s.chars().count());

    // Labels: fit as many as possible, show "+N more" if truncated
    let id_display_width = format!("{:<8}", &story.id).chars().count();
    // Rough budget for labels: total width - cursor(2) - id(8) - gap(1) - min_title(10) - non_label_right
    let label_budget = width.saturating_sub(2 + id_display_width + 1 + 10 + non_label_right);
    let mut label_parts: Vec<(String, ratatui::style::Style)> = Vec::new();
    let mut label_width_used = 0usize;
    let mut labels_shown = 0usize;
    for label in &story.labels {
        let label_str = format!(" [{label}]");
        let label_len = label_str.chars().count();
        let remaining = story.labels.len() - labels_shown;
        // Reserve space for "+N more" if there are more labels after this
        let more_suffix_width = if remaining > 1 {
            format!(" +{} more", remaining - 1).chars().count()
        } else {
            0
        };
        if label_width_used + label_len + more_suffix_width <= label_budget || labels_shown == 0 {
            label_parts.push((label_str, theme.labels));
            label_width_used += label_len;
            labels_shown += 1;
        } else {
            let more_count = story.labels.len() - labels_shown;
            label_parts.push((format!(" +{more_count} more"), theme.section_count));
            break;
        }
    }

    right_parts.extend(label_parts);
    if let Some(part) = assignee_part {
        right_parts.push(part);
    }
    if let Some(part) = blocked_part {
        right_parts.push(part);
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
            story_type: None,
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
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
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

    // --- Task 5.2 Tests: Mouse support ---

    #[test]
    fn mouse_click_selects_row() {
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
        board.render_area = Rect::new(0, 1, 80, 22); // Simulated board area
        assert_eq!(board.cursor, 0);

        // Click on row at y=3 (area.y=1, so click_y=2, row_index = scroll_offset(0) + 2 = 2)
        // Row 2 should be SH-1 story row
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 3,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let actions = board.handle_mouse(mouse, &state);
        assert!(actions.is_empty()); // single click just selects
        assert_eq!(board.cursor, 2);
    }

    #[test]
    fn mouse_double_click_opens_detail() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.render_area = Rect::new(0, 1, 80, 22);

        // First click on SH-1 row (row index 1 = story row)
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 2, // area.y=1, click_y=1, row_index=1 (SH-1)
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        board.handle_mouse(mouse, &state);
        assert_eq!(board.cursor, 1);

        // Second click within 300ms on the same row (simulated by last_click being recent)
        // last_click was set by the first click
        let actions = board.handle_mouse(mouse, &state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::OpenDetail(id) if id == "SH-1"));
    }

    #[test]
    fn mouse_double_click_on_header_does_not_open_detail() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.render_area = Rect::new(0, 1, 80, 22);

        // Double click on header row (row index 0 = section header)
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 1, // area.y=1, click_y=0, row_index=0 (header)
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        board.handle_mouse(mouse, &state);
        let actions = board.handle_mouse(mouse, &state);
        // Double click on header should NOT emit OpenDetail
        assert!(actions.is_empty());
    }

    #[test]
    fn mouse_scroll_moves_cursor() {
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
        board.render_area = Rect::new(0, 1, 80, 22);
        assert_eq!(board.cursor, 0);

        // Scroll down
        let scroll_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        board.handle_mouse(scroll_down, &state);
        assert_eq!(board.cursor, 1);

        // Scroll up
        let scroll_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        board.handle_mouse(scroll_up, &state);
        assert_eq!(board.cursor, 0);

        // Scroll up at 0 stays at 0
        board.handle_mouse(scroll_up, &state);
        assert_eq!(board.cursor, 0);
    }

    #[test]
    fn mouse_click_outside_board_area_ignored() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.render_area = Rect::new(0, 1, 80, 22);

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 0, // Above board area (area.y=1)
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let actions = board.handle_mouse(mouse, &state);
        assert!(actions.is_empty());
        assert_eq!(board.cursor, 0); // unchanged
    }

    #[test]
    fn double_click_timing_expired_does_not_open() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.render_area = Rect::new(0, 1, 80, 22);

        // Simulate a click that happened long ago
        board.last_click = Some((Instant::now() - std::time::Duration::from_millis(500), 1));

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 2, // row_index=1 (SH-1)
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        // This should NOT be a double-click since last click was > 300ms ago
        let actions = board.handle_mouse(mouse, &state);
        assert!(actions.is_empty());
        assert_eq!(board.cursor, 1);
    }

    #[test]
    fn very_small_terminal_does_not_panic() {
        // Task 5.5: Resize handling - components shouldn't panic with tiny viewports
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "First story with a very long title"),
                test_snapshot("SH-2", "in-progress", "Second"),
            ],
            "SH".to_string(),
            vec![],
        );
        let mut state = make_state(data);
        state.terminal_size = (20, 5); // Very small terminal

        let mut board = Board::new();
        // The board should handle small viewport gracefully
        board.on_state_change(&state);

        // Navigate should not panic
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            &state,
        );

        // Zero-size viewport
        state.terminal_size = (0, 0);
        board.on_state_change(&state);
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
    }

    #[test]
    fn hit_region_matching_finds_correct_target() {
        // Test that hit regions populated during render can be used to find the right target
        use super::super::HitTarget;

        let regions = [
            HitRegion {
                rect: Rect::new(0, 1, 80, 1),
                target: HitTarget::SectionHeader {
                    state_slug: "todo".to_string(),
                },
            },
            HitRegion {
                rect: Rect::new(0, 2, 80, 1),
                target: HitTarget::StoryRow {
                    id: "SH-1".to_string(),
                },
            },
            HitRegion {
                rect: Rect::new(0, 3, 80, 1),
                target: HitTarget::StoryRow {
                    id: "SH-2".to_string(),
                },
            },
        ];

        // Click at (10, 2) should match SH-1
        let col = 10u16;
        let row = 2u16;
        let matched = regions.iter().find(|r| {
            col >= r.rect.x
                && col < r.rect.x + r.rect.width
                && row >= r.rect.y
                && row < r.rect.y + r.rect.height
        });
        assert!(matched.is_some());
        assert!(
            matches!(&matched.unwrap().target, HitTarget::StoryRow { id } if id == "SH-1")
        );

        // Click at (10, 1) should match todo header
        let row = 1u16;
        let matched = regions.iter().find(|r| {
            col >= r.rect.x
                && col < r.rect.x + r.rect.width
                && row >= r.rect.y
                && row < r.rect.y + r.rect.height
        });
        assert!(matched.is_some());
        assert!(
            matches!(&matched.unwrap().target, HitTarget::SectionHeader { state_slug } if state_slug == "todo")
        );

        // Click at (10, 10) should match nothing (out of range)
        let row = 10u16;
        let matched = regions.iter().find(|r| {
            col >= r.rect.x
                && col < r.rect.x + r.rect.width
                && row >= r.rect.y
                && row < r.rect.y + r.rect.height
        });
        assert!(matched.is_none());
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

    // =======================================================================
    // QA: Cursor-out-of-bounds after filter changes
    // =======================================================================

    #[test]
    fn cursor_clamps_after_filter_reduces_rows() {
        // The #1 crash scenario: cursor at row 5, filter removes stories,
        // now there are only 3 rows. Without clamping, accessing row 5 panics.
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "Alpha"),
                test_snapshot("SH-2", "todo", "Beta"),
                test_snapshot("SH-3", "in-progress", "Gamma"),
                test_snapshot("SH-4", "in-progress", "Delta"),
                test_snapshot("SH-5", "review", "Epsilon"),
            ],
            "SH".to_string(),
            vec![],
        );
        let mut state = make_state(data);
        let mut board = Board::new();

        // Place cursor on last visible row
        let rows = board.build_visible_rows(&state);
        board.cursor = rows.len() - 1; // index 8 (3 headers + 5 stories)

        // Now apply a filter that eliminates most stories
        use crate::tui::action::FilterSpec;
        state.filters.push(FilterSpec {
            text: Some("Alpha".to_string()),
            ..Default::default()
        });
        board.on_state_change(&state);

        // After filter: 3 headers + 1 story = 4 rows. Cursor must clamp.
        let new_rows = board.build_visible_rows(&state);
        assert!(board.cursor < new_rows.len(),
            "cursor {} must be < row count {}", board.cursor, new_rows.len());
    }

    #[test]
    fn cursor_clamps_after_filter_removes_all_stories() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "Alpha"),
                test_snapshot("SH-2", "in-progress", "Beta"),
            ],
            "SH".to_string(),
            vec![],
        );
        let mut state = make_state(data);
        let mut board = Board::new();
        board.cursor = 3; // on the in-progress story row

        // Filter that matches nothing
        use crate::tui::action::FilterSpec;
        state.filters.push(FilterSpec {
            text: Some("ZZZZZ_NO_MATCH".to_string()),
            ..Default::default()
        });
        board.on_state_change(&state);

        let new_rows = board.build_visible_rows(&state);
        // Should have 3 section headers only
        assert_eq!(new_rows.len(), 3);
        assert!(board.cursor < new_rows.len());
    }

    #[test]
    fn navigation_after_filter_does_not_panic() {
        // Simulate: have stories, filter to subset, navigate with j/k/g/G
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "Alpha"),
                test_snapshot("SH-2", "todo", "Beta"),
                test_snapshot("SH-3", "in-progress", "Gamma"),
            ],
            "SH".to_string(),
            vec![],
        );
        let mut state = make_state(data);
        let mut board = Board::new();

        // Apply filter
        use crate::tui::action::FilterSpec;
        state.filters.push(FilterSpec {
            text: Some("Alpha".to_string()),
            ..Default::default()
        });
        board.on_state_change(&state);

        // Navigate extensively -- none of these should panic
        for _ in 0..20 {
            board.handle_key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &state,
            );
        }
        for _ in 0..20 {
            board.handle_key(
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
                &state,
            );
        }
        board.handle_key(
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            &state,
        );
        // Section jumps
        board.handle_key(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            &state,
        );
    }

    // =======================================================================
    // QA: Filter + collapse interaction
    // =======================================================================

    #[test]
    fn filter_then_collapse_then_clear_filter() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "Alpha"),
                test_snapshot("SH-2", "todo", "Beta"),
                test_snapshot("SH-3", "in-progress", "Gamma"),
            ],
            "SH".to_string(),
            vec![],
        );
        let mut state = make_state(data);
        let mut board = Board::new();

        // Filter to Alpha only
        use crate::tui::action::FilterSpec;
        state.filters.push(FilterSpec {
            text: Some("Alpha".to_string()),
            ..Default::default()
        });
        board.on_state_change(&state);

        // Collapse todo section
        board.cursor = 0; // todo header
        board.handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &state,
        );
        assert!(board.collapsed.contains("todo"));

        // Clear filter -- todo section is still collapsed
        state.filters.clear();
        board.on_state_change(&state);

        let rows = board.build_visible_rows(&state);
        // todo collapsed: header only. in-progress: header + Gamma. review: header.
        // Total: 3 headers + 1 story = 4
        assert_eq!(rows.len(), 4);
        assert!(board.cursor < rows.len());
    }

    // =======================================================================
    // QA: All sections collapsed
    // =======================================================================

    #[test]
    fn all_sections_collapsed_navigation() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "Alpha"),
                test_snapshot("SH-2", "in-progress", "Beta"),
                test_snapshot("SH-3", "review", "Gamma"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();

        // Collapse all sections
        board.collapsed.insert("todo".to_string());
        board.collapsed.insert("in-progress".to_string());
        board.collapsed.insert("review".to_string());

        let rows = board.build_visible_rows(&state);
        // Only 3 section headers, no story rows
        assert_eq!(rows.len(), 3);

        // Navigate with j/k -- should work on headers only
        board.cursor = 0;
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 1);
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 2);

        // Can't go past last
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(board.cursor, 2);

        // Enter on header should do nothing (no story)
        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &state,
        );
        assert!(actions.is_empty());

        // > on header should do nothing (no story)
        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT),
            &state,
        );
        assert!(actions.is_empty());
    }

    // =======================================================================
    // QA: Empty board edge cases
    // =======================================================================

    #[test]
    fn empty_board_no_states_at_all() {
        // Edge case: a project with no states defined (empty states.toml)
        let data = DataStore::from_test_data(
            vec![], // no states
            vec![], // no stories
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();

        let rows = board.build_visible_rows(&state);
        assert_eq!(rows.len(), 0);

        // All navigation should be no-ops, never panic
        board.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            &state,
        );
        board.handle_key(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            &state,
        );

        assert_eq!(board.cursor, 0);
    }

    #[test]
    fn empty_board_n_still_opens_create_form() {
        let data = DataStore::from_test_data(
            vec![], // no states
            vec![], // no stories
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::OpenCreateForm));
    }

    #[test]
    fn empty_board_slash_still_focuses_filter_bar() {
        let data = DataStore::from_test_data(
            vec![], // no states
            vec![], // no stories
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::FocusFilterBar));
    }

    // =======================================================================
    // QA: State movement at boundaries
    // =======================================================================

    #[test]
    fn move_forward_from_last_closed_state_only() {
        // Project with only closed states (weird but possible)
        use crate::domain::StateDef;
        let data = DataStore::from_test_data(
            vec![
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
        let state = make_state(data);
        let board = Board::new();

        // No open states => no visible rows
        let rows = board.build_visible_rows(&state);
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn move_forward_on_header_row_does_nothing() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.cursor = 0; // on "todo" section header

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT),
            &state,
        );
        assert!(actions.is_empty(), "> on header should do nothing");

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT),
            &state,
        );
        assert!(actions.is_empty(), "< on header should do nothing");
    }

    // =======================================================================
    // QA: Collapse with cursor on story being hidden
    // =======================================================================

    #[test]
    fn collapse_section_cursor_was_on_story_inside() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo", "Alpha"),
                test_snapshot("SH-2", "todo", "Beta"),
                test_snapshot("SH-3", "in-progress", "Gamma"),
            ],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();

        // Rows: [0] todo hdr, [1] SH-1, [2] SH-2, [3] in-progress hdr, [4] SH-3, [5] review hdr
        // Move cursor to todo header and collapse
        board.cursor = 0;
        board.handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &state,
        );
        // After collapse: [0] todo hdr, [1] in-progress hdr, [2] SH-3, [3] review hdr
        assert!(board.collapsed.contains("todo"));
        assert!(board.cursor < 4, "cursor should be clamped to new row count");
    }

    // =======================================================================
    // QA: on_state_change with 0-height terminal
    // =======================================================================

    #[test]
    fn on_state_change_zero_terminal_height() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "Alpha")],
            "SH".to_string(),
            vec![],
        );
        let mut state = make_state(data);
        state.terminal_size = (80, 0); // height = 0

        let mut board = Board::new();
        // Should not panic
        board.on_state_change(&state);
        assert_eq!(board.cursor, 0);
    }

    // =======================================================================
    // QA: scroll_offset integrity
    // =======================================================================

    #[test]
    fn scroll_offset_stays_valid_on_rapid_navigation() {
        let mut stories = Vec::new();
        for i in 1..=20 {
            stories.push(test_snapshot(
                &format!("SH-{i}"),
                "todo",
                &format!("Story {i}"),
            ));
        }
        let data = DataStore::from_test_data(
            test_states(),
            stories,
            "SH".to_string(),
            vec![],
        );
        let mut state = make_state(data);
        state.terminal_size = (80, 10); // small viewport

        let mut board = Board::new();
        // Navigate to bottom
        for _ in 0..30 {
            board.handle_key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &state,
            );
        }
        // Navigate back to top
        for _ in 0..30 {
            board.handle_key(
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
                &state,
            );
        }
        // scroll_offset should be 0 (or at least valid)
        let rows = board.build_visible_rows(&state);
        assert!(board.cursor < rows.len());
        assert!(board.scroll_offset <= board.cursor);
    }

    // =======================================================================
    // Drag-and-drop tests (Phase 2 mouse)
    // =======================================================================

    #[test]
    fn drag_pending_on_mousedown_story_row() {
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
        board.render_area = Rect::new(0, 1, 80, 22);

        // Click on SH-1 story row (row index 1, screen row = area.y + 1 = 2)
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(mouse, &state);
        assert!(
            matches!(
                &board.drag_state,
                DragState::Pending { story_id, origin_row: 1, start_col: 10, start_row: 2 }
                if story_id == "SH-1"
            ),
            "mousedown on story row should set Pending drag state"
        );
    }

    #[test]
    fn drag_threshold_prevents_accidental_drag() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.render_area = Rect::new(0, 1, 80, 22);

        // Click on SH-1
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(down, &state);

        // Drag only 1 cell (Manhattan distance = 1, below threshold of 3)
        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 11,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(drag, &state);
        assert!(
            matches!(&board.drag_state, DragState::Pending { .. }),
            "small movement should stay Pending"
        );
    }

    #[test]
    fn drag_active_after_threshold() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.render_area = Rect::new(0, 1, 80, 22);
        // Pre-populate hit_regions so find_section_at works
        board.hit_regions = vec![
            HitRegion {
                rect: Rect::new(0, 1, 80, 1),
                target: HitTarget::SectionHeader {
                    state_slug: "todo".to_string(),
                },
            },
            HitRegion {
                rect: Rect::new(0, 2, 80, 1),
                target: HitTarget::StoryRow {
                    id: "SH-1".to_string(),
                },
            },
        ];

        // Click on SH-1
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(down, &state);

        // Drag more than 3 cells (Manhattan distance = 4)
        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 10,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(drag, &state);
        assert!(
            matches!(&board.drag_state, DragState::Dragging { story_id, .. } if story_id == "SH-1"),
            "movement exceeding threshold should transition to Dragging"
        );
    }

    #[test]
    fn drag_drop_on_section_header_emits_move() {
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
        board.render_area = Rect::new(0, 1, 80, 22);
        // Rows: [0] todo hdr (y=1), [1] SH-1 (y=2), [2] in-progress hdr (y=3), [3] SH-2 (y=4), [4] review hdr (y=5)
        board.hit_regions = vec![
            HitRegion {
                rect: Rect::new(0, 1, 80, 1),
                target: HitTarget::SectionHeader {
                    state_slug: "todo".to_string(),
                },
            },
            HitRegion {
                rect: Rect::new(0, 2, 80, 1),
                target: HitTarget::StoryRow {
                    id: "SH-1".to_string(),
                },
            },
            HitRegion {
                rect: Rect::new(0, 3, 80, 1),
                target: HitTarget::SectionHeader {
                    state_slug: "in-progress".to_string(),
                },
            },
            HitRegion {
                rect: Rect::new(0, 4, 80, 1),
                target: HitTarget::StoryRow {
                    id: "SH-2".to_string(),
                },
            },
            HitRegion {
                rect: Rect::new(0, 5, 80, 1),
                target: HitTarget::SectionHeader {
                    state_slug: "review".to_string(),
                },
            },
        ];

        // Click on SH-1
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(down, &state);

        // Drag to in-progress header row (y=3), distance > 3 not needed if we
        // first exceed threshold then land on header. Let's exceed threshold first.
        let drag_far = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 10,
            row: 6, // distance = 4 > 3
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(drag_far, &state);

        // Drag to land on in-progress header
        let drag_target = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 10,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(drag_target, &state);

        // Verify current_target_section is set
        assert!(
            matches!(
                &board.drag_state,
                DragState::Dragging { current_target_section: Some(slug), .. }
                if slug == "in-progress"
            ),
            "dragging over section header should set current_target_section"
        );

        // Drop
        let up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 10,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        let actions = board.handle_mouse(up, &state);
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(
                &actions[0],
                Action::MoveStory { id, target_state }
                if id == "SH-1" && target_state == "in-progress"
            ),
            "drop on section header should emit MoveStory"
        );
        assert_eq!(board.drag_state, DragState::Idle);
    }

    #[test]
    fn drag_cancel_on_esc() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.drag_state = DragState::Dragging {
            story_id: "SH-1".to_string(),
            origin_row: 1,
            current_target_section: Some("in-progress".to_string()),
        };

        let actions = board.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &state,
        );
        assert!(actions.is_empty());
        assert_eq!(board.drag_state, DragState::Idle);
    }

    #[test]
    fn drag_cancel_on_mouseup_no_target() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.drag_state = DragState::Dragging {
            story_id: "SH-1".to_string(),
            origin_row: 1,
            current_target_section: None,
        };

        let up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 40,
            row: 15,
            modifiers: KeyModifiers::NONE,
        };
        let actions = board.handle_mouse(up, &state);
        assert!(actions.is_empty(), "drop with no target should cancel");
        assert_eq!(board.drag_state, DragState::Idle);
    }

    #[test]
    fn click_preserved_when_no_drag() {
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
        board.render_area = Rect::new(0, 1, 80, 22);

        // Click on SH-2 (row index 2, screen row = 3)
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(down, &state);
        assert_eq!(board.cursor, 2, "click should select the row");

        // Release without drag
        let up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 10,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        let actions = board.handle_mouse(up, &state);
        assert!(actions.is_empty(), "simple click-release should not emit actions");
        assert_eq!(board.drag_state, DragState::Idle);
        assert_eq!(board.cursor, 2, "cursor should remain on clicked row");
    }

    #[test]
    fn double_click_preserved_when_no_drag() {
        let data = DataStore::from_test_data(
            test_states(),
            vec![test_snapshot("SH-1", "todo", "First")],
            "SH".to_string(),
            vec![],
        );
        let state = make_state(data);
        let mut board = Board::new();
        board.render_area = Rect::new(0, 1, 80, 22);

        // First click on SH-1 (row index 1, screen row = 2)
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        board.handle_mouse(mouse, &state);

        // Second click (double-click) on same row
        let actions = board.handle_mouse(mouse, &state);
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], Action::OpenDetail(id) if id == "SH-1"),
            "double-click should still open detail"
        );
    }
}
