use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::domain::{Priority, StorySnapshot, SuperState, ready_order};
use crate::tui::action::{Action, View};
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::{Component, HitRegion};

/// A selectable item in the dashboard.
#[derive(Debug, Clone)]
enum DashboardItem {
    /// A non-interactive header/label line.
    Header,
    /// The "View Board" action line.
    ViewBoard,
    /// A story row (in ready or recent sections).
    Story { id: String },
    /// A blank separator.
    Blank,
}

/// The dashboard component: project summary home screen.
pub struct Dashboard {
    /// Flat list of renderable items.
    items: Vec<DashboardItem>,
    /// Currently selected item index.
    cursor: usize,
    pub hit_regions: Vec<HitRegion>,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Dashboard {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            cursor: 0,
            hit_regions: Vec::new(),
        }
    }

    /// Rebuild the flat item list from current data.
    fn build_items(&mut self, state: &AppState) {
        self.items.clear();

        // Title section
        self.items.push(DashboardItem::Header); // project name + counts
        self.items.push(DashboardItem::Blank);

        // State breakdown header
        self.items.push(DashboardItem::Header); // "Stories by State"
        for _state_def in &state.data.states {
            self.items.push(DashboardItem::Header); // each state line
        }
        self.items.push(DashboardItem::Blank);

        // Priority breakdown header
        self.items.push(DashboardItem::Header); // "Priority Breakdown"
        // 5 priority levels
        for _ in 0..5 {
            self.items.push(DashboardItem::Header);
        }
        self.items.push(DashboardItem::Blank);

        // Ready stories
        self.items.push(DashboardItem::Header); // "Ready Stories"
        let ready = ready_stories(&state.data.stories);
        if ready.is_empty() {
            self.items.push(DashboardItem::Header); // "(none)"
        } else {
            for story in ready.iter().take(5) {
                self.items.push(DashboardItem::Story {
                    id: story.id.clone(),
                });
            }
        }
        self.items.push(DashboardItem::Blank);

        // Recent activity
        self.items.push(DashboardItem::Header); // "Recent Activity"
        let recent = recent_stories(&state.data.stories);
        if recent.is_empty() {
            self.items.push(DashboardItem::Header); // "(none)"
        } else {
            for story in recent.iter().take(5) {
                self.items.push(DashboardItem::Story {
                    id: story.id.clone(),
                });
            }
        }
        self.items.push(DashboardItem::Blank);

        // View Board action
        self.items.push(DashboardItem::ViewBoard);

        // Clamp cursor
        if !self.items.is_empty() && self.cursor >= self.items.len() {
            self.cursor = self.items.len() - 1;
        }
    }

    /// Find the next selectable item index at or after `from`.
    fn next_selectable(&self, from: usize) -> Option<usize> {
        (from..self.items.len()).find(|&i| self.is_selectable(i))
    }

    /// Find the previous selectable item index before `from`.
    fn prev_selectable(&self, from: usize) -> Option<usize> {
        (0..from).rev().find(|&i| self.is_selectable(i))
    }

    fn is_selectable(&self, idx: usize) -> bool {
        matches!(
            self.items.get(idx),
            Some(DashboardItem::Story { .. } | DashboardItem::ViewBoard)
        )
    }
}

impl Component for Dashboard {
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        self.build_items(state);

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(next) = self.next_selectable(self.cursor + 1) {
                    self.cursor = next;
                }
                vec![]
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(prev) = self.prev_selectable(self.cursor) {
                    self.cursor = prev;
                }
                vec![]
            }
            KeyCode::Enter => {
                if let Some(item) = self.items.get(self.cursor) {
                    match item {
                        DashboardItem::Story { id } => {
                            return vec![Action::OpenDetail(id.clone())];
                        }
                        DashboardItem::ViewBoard => {
                            return vec![Action::SwitchView(View::Board)];
                        }
                        _ => {}
                    }
                }
                vec![]
            }
            KeyCode::Char('n') => vec![Action::OpenCreateForm],
            KeyCode::Char('2') => vec![Action::SwitchView(View::Board)],
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent, _state: &AppState) -> Vec<Action> {
        vec![]
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        let theme = Theme::from_env();
        let mut lines: Vec<Line> = Vec::new();

        // --- Project title and counts ---
        let project_name = if state.data.prefix.is_empty() {
            "Project".to_string()
        } else {
            state.data.prefix.clone()
        };
        let total_open = state.data.stories.len();

        lines.push(Line::from(vec![
            Span::styled(format!("  {project_name}"), theme.section_header),
            Span::styled(format!("  {total_open} open stories"), theme.section_count),
        ]));
        lines.push(Line::from(""));

        // Show empty state message if no stories at all
        if total_open == 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  No stories yet. Press n to create one.",
                theme.section_count,
            )));
            lines.push(Line::from(""));
            frame.render_widget(Paragraph::new(lines), area);
            return;
        }

        // --- Stories by State ---
        lines.push(Line::from(Span::styled(
            "  Stories by State",
            theme.section_header,
        )));
        let state_counts = stories_per_state(&state.data.stories, &state.data.states);
        for (slug, count) in &state_counts {
            lines.push(Line::from(vec![
                Span::styled(format!("    {slug:<20}"), theme.story_title),
                Span::styled(format!("{count}"), theme.section_count),
            ]));
        }
        lines.push(Line::from(""));

        // --- Priority Breakdown ---
        lines.push(Line::from(Span::styled(
            "  Priority Breakdown",
            theme.section_header,
        )));
        let pri_counts = priority_breakdown(&state.data.stories);
        for (label, count, style) in [
            ("Critical", pri_counts.critical, theme.priority_critical),
            ("High", pri_counts.high, theme.priority_high),
            ("Medium", pri_counts.medium, theme.priority_medium),
            ("Low", pri_counts.low, theme.priority_low),
            ("None", pri_counts.none, theme.section_count),
        ] {
            lines.push(Line::from(vec![
                Span::styled(format!("    {label:<20}"), style),
                Span::styled(format!("{count}"), theme.section_count),
            ]));
        }
        lines.push(Line::from(""));

        // --- Ready Stories (top 5) ---
        lines.push(Line::from(Span::styled(
            "  Ready Stories",
            theme.section_header,
        )));
        let ready = ready_stories(&state.data.stories);
        if ready.is_empty() {
            lines.push(Line::from(Span::styled("    (none)", theme.section_count)));
        } else {
            // Track the line index where ready stories start so we can check cursor
            let ready_start = lines.len();
            for (i, story) in ready.iter().take(5).enumerate() {
                let is_selected = self.is_ready_story_selected(state, i);
                lines.push(render_story_line(story, is_selected, &theme));
            }
            let _ = ready_start; // used for cursor tracking
        }
        lines.push(Line::from(""));

        // --- Recent Activity (last 5) ---
        lines.push(Line::from(Span::styled(
            "  Recent Activity",
            theme.section_header,
        )));
        let recent = recent_stories(&state.data.stories);
        if recent.is_empty() {
            lines.push(Line::from(Span::styled("    (none)", theme.section_count)));
        } else {
            for (i, story) in recent.iter().take(5).enumerate() {
                let is_selected = self.is_recent_story_selected(state, i);
                lines.push(render_story_line(story, is_selected, &theme));
            }
        }
        lines.push(Line::from(""));

        // --- View Board action ---
        let board_selected = matches!(self.items.get(self.cursor), Some(DashboardItem::ViewBoard));
        if board_selected {
            lines.push(Line::from(vec![
                Span::styled("  > ", theme.cursor),
                Span::styled("View Board (2)", theme.cursor),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                "    View Board (2)",
                theme.section_count,
            )));
        }

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn on_state_change(&mut self, state: &AppState) {
        self.build_items(state);
    }

    fn hit_regions(&self) -> &[HitRegion] {
        &self.hit_regions
    }
}

impl Dashboard {
    /// Check if the ready story at index `i` is currently selected by the cursor.
    fn is_ready_story_selected(&self, state: &AppState, story_idx: usize) -> bool {
        let ready = ready_stories(&state.data.stories);
        if story_idx >= ready.len() {
            return false;
        }
        if let Some(DashboardItem::Story { id }) = self.items.get(self.cursor) {
            // Walk the items to find the ready stories section and match index
            return *id == ready[story_idx].id && self.is_cursor_in_ready_section(state);
        }
        false
    }

    /// Check if the recent story at index `i` is currently selected by the cursor.
    fn is_recent_story_selected(&self, state: &AppState, story_idx: usize) -> bool {
        let recent = recent_stories(&state.data.stories);
        if story_idx >= recent.len() {
            return false;
        }
        if let Some(DashboardItem::Story { id }) = self.items.get(self.cursor) {
            return *id == recent[story_idx].id && self.is_cursor_in_recent_section(state);
        }
        false
    }

    /// Check if cursor is in the "ready" section of items.
    fn is_cursor_in_ready_section(&self, _state: &AppState) -> bool {
        // Find the boundary: ready stories come before recent stories.
        // Walk backwards from cursor to find the nearest header-like boundary.
        // Simple approach: count blank separators to determine section.
        let mut blank_count = 0;
        for i in (0..self.cursor).rev() {
            if matches!(self.items.get(i), Some(DashboardItem::Blank)) {
                blank_count += 1;
                if blank_count >= 1 {
                    break;
                }
            }
        }
        // Ready section is after 3 blanks (title, states, priority)
        // Recent section is after 4 blanks
        // Count blanks before cursor
        let blanks_before: usize = self.items[..self.cursor]
            .iter()
            .filter(|item| matches!(item, DashboardItem::Blank))
            .count();
        blanks_before == 3
    }

    /// Check if cursor is in the "recent" section of items.
    fn is_cursor_in_recent_section(&self, _state: &AppState) -> bool {
        let blanks_before: usize = self.items[..self.cursor]
            .iter()
            .filter(|item| matches!(item, DashboardItem::Blank))
            .count();
        blanks_before == 4
    }
}

/// Render a story as a dashboard line.
fn render_story_line<'a>(story: &StorySnapshot, is_selected: bool, theme: &Theme) -> Line<'a> {
    let cursor_mark = if is_selected { "  > " } else { "    " };
    let priority_sym = match story.priority {
        Priority::Critical => "!!! ",
        Priority::High => "!!  ",
        Priority::Medium => "!   ",
        Priority::Low => ".   ",
        Priority::None => "    ",
    };

    let mut spans = vec![
        Span::styled(
            cursor_mark.to_string(),
            if is_selected {
                theme.cursor
            } else {
                theme.story_id
            },
        ),
        Span::styled(format!("{:<8}", story.id), theme.story_id),
        Span::raw(" "),
        Span::styled(story.title.clone(), theme.story_title),
        Span::raw("  "),
        Span::styled(
            priority_sym.to_string(),
            match story.priority {
                Priority::Critical => theme.priority_critical,
                Priority::High => theme.priority_high,
                Priority::Medium => theme.priority_medium,
                Priority::Low => theme.priority_low,
                Priority::None => theme.section_count,
            },
        ),
    ];

    if is_selected {
        for span in &mut spans {
            span.style = span.style.patch(theme.selected_row);
        }
    }

    Line::from(spans)
}

/// Count stories per state.
pub fn stories_per_state(
    stories: &[StorySnapshot],
    states: &[crate::domain::StateDef],
) -> Vec<(String, usize)> {
    states
        .iter()
        .map(|s| {
            let count = stories.iter().filter(|st| st.state == s.slug).count();
            (s.slug.clone(), count)
        })
        .collect()
}

/// Count stories by priority level.
pub struct PriorityBreakdown {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub none: usize,
}

pub fn priority_breakdown(stories: &[StorySnapshot]) -> PriorityBreakdown {
    let mut b = PriorityBreakdown {
        critical: 0,
        high: 0,
        medium: 0,
        low: 0,
        none: 0,
    };
    for story in stories {
        match story.priority {
            Priority::Critical => b.critical += 1,
            Priority::High => b.high += 1,
            Priority::Medium => b.medium += 1,
            Priority::Low => b.low += 1,
            Priority::None => b.none += 1,
        }
    }
    b
}

/// Get stories that are "ready" (no awaiting), ranked the way `story next`
/// ranks them: [`domain::ready_order`](crate::domain::ready_order), priority
/// then story number. Sorting by priority alone (as this used to) has no
/// second key at all, so two same-priority stories fell back to whatever
/// order `stories` arrived in — the same tie SH-63 fixed on the CLI side.
pub fn ready_stories(stories: &[StorySnapshot]) -> Vec<&StorySnapshot> {
    let mut ready: Vec<&StorySnapshot> = stories
        .iter()
        .filter(|s| s.awaiting.is_none() && s.superstate == SuperState::Open)
        .collect();
    ready.sort_by(|a, b| ready_order(a, b));
    ready
}

/// Get the most recently updated stories, sorted by updated_at descending.
pub fn recent_stories(stories: &[StorySnapshot]) -> Vec<&StorySnapshot> {
    let mut recent: Vec<&StorySnapshot> = stories.iter().collect();
    recent.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    recent.truncate(5);
    recent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Priority, StateDef, StorySnapshot, SuperState};

    fn test_states() -> Vec<StateDef> {
        vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: None,
            },
            StateDef {
                slug: "in-progress".to_string(),
                super_state: SuperState::Open,
                role: Some("active".to_string()),
                description: None,
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: None,
            },
        ]
    }

    fn make_snapshot(
        id: &str,
        state: &str,
        title: &str,
        priority: Priority,
        awaiting: Option<&str>,
        updated_at: &str,
    ) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: title.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            state: state.to_string(),
            superstate: if state == "done" {
                SuperState::Closed
            } else {
                SuperState::Open
            },
            assignee: None,
            awaiting: awaiting.map(|s| s.to_string()),
            comments: vec![],
            relationships: vec![],
            priority,
            labels: vec![],
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
        }
    }

    #[test]
    fn stories_per_state_counts_correctly() {
        let stories = vec![
            make_snapshot(
                "SH-1",
                "todo",
                "A",
                Priority::None,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-2",
                "todo",
                "B",
                Priority::None,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-3",
                "in-progress",
                "C",
                Priority::None,
                None,
                "2026-01-01T00:00:00Z",
            ),
        ];
        let states = test_states();
        let counts = stories_per_state(&stories, &states);
        assert_eq!(counts[0], ("todo".to_string(), 2));
        assert_eq!(counts[1], ("in-progress".to_string(), 1));
        assert_eq!(counts[2], ("done".to_string(), 0));
    }

    #[test]
    fn priority_breakdown_counts_correctly() {
        let stories = vec![
            make_snapshot(
                "SH-1",
                "todo",
                "A",
                Priority::Critical,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-2",
                "todo",
                "B",
                Priority::High,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-3",
                "todo",
                "C",
                Priority::High,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-4",
                "todo",
                "D",
                Priority::None,
                None,
                "2026-01-01T00:00:00Z",
            ),
        ];
        let b = priority_breakdown(&stories);
        assert_eq!(b.critical, 1);
        assert_eq!(b.high, 2);
        assert_eq!(b.medium, 0);
        assert_eq!(b.low, 0);
        assert_eq!(b.none, 1);
    }

    #[test]
    fn ready_stories_excludes_blocked() {
        let stories = vec![
            make_snapshot(
                "SH-1",
                "todo",
                "A",
                Priority::High,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-2",
                "todo",
                "B",
                Priority::Low,
                Some("waiting"),
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-3",
                "todo",
                "C",
                Priority::Critical,
                None,
                "2026-01-01T00:00:00Z",
            ),
        ];
        let ready = ready_stories(&stories);
        assert_eq!(ready.len(), 2);
        // Sorted by priority: Critical first, then High
        assert_eq!(ready[0].id, "SH-3");
        assert_eq!(ready[1].id, "SH-1");
    }

    #[test]
    fn ready_stories_excludes_closed() {
        let stories = vec![
            make_snapshot(
                "SH-1",
                "todo",
                "A",
                Priority::High,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-2",
                "done",
                "B",
                Priority::Low,
                None,
                "2026-01-01T00:00:00Z",
            ),
        ];
        let ready = ready_stories(&stories);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "SH-1");
    }

    /// Same priority, same `created_at` (every fixture snapshot shares one) —
    /// exactly the tie a priority-only sort (this function's shape before
    /// SH-63) has no second key for. `ready_order` breaks it by story number.
    #[test]
    fn ready_stories_breaks_a_priority_tie_by_story_number() {
        let stories = vec![
            make_snapshot(
                "SH-10",
                "todo",
                "Second in number",
                Priority::High,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-9",
                "todo",
                "First in number",
                Priority::High,
                None,
                "2026-01-01T00:00:00Z",
            ),
        ];
        let ready = ready_stories(&stories);
        assert_eq!(
            ready.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["SH-9", "SH-10"],
            "the lower-numbered story wins, not the input order or the id string"
        );
    }

    #[test]
    fn recent_stories_sorted_by_updated_at() {
        let stories = vec![
            make_snapshot(
                "SH-1",
                "todo",
                "A",
                Priority::None,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-2",
                "todo",
                "B",
                Priority::None,
                None,
                "2026-01-03T00:00:00Z",
            ),
            make_snapshot(
                "SH-3",
                "todo",
                "C",
                Priority::None,
                None,
                "2026-01-02T00:00:00Z",
            ),
        ];
        let recent = recent_stories(&stories);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].id, "SH-2"); // most recent
        assert_eq!(recent[1].id, "SH-3");
        assert_eq!(recent[2].id, "SH-1"); // oldest
    }

    #[test]
    fn recent_stories_limited_to_5() {
        let mut stories = Vec::new();
        for i in 1..=10 {
            stories.push(make_snapshot(
                &format!("SH-{i}"),
                "todo",
                &format!("Story {i}"),
                Priority::None,
                None,
                &format!("2026-01-{i:02}T00:00:00Z"),
            ));
        }
        let recent = recent_stories(&stories);
        assert_eq!(recent.len(), 5);
    }

    // =======================================================================
    // QA: Empty project behavior
    // =======================================================================

    fn make_app_state(stories: Vec<StorySnapshot>) -> crate::tui::state::AppState {
        use crate::tui::action::View;
        use crate::tui::data::DataStore;
        use crate::tui::focus::{FocusStack, FocusTarget};
        let data = DataStore::from_test_data(test_states(), stories, "SH".to_string(), vec![]);
        crate::tui::state::AppState {
            data,
            focus: FocusStack::new(FocusTarget::Dashboard),
            view: View::Dashboard,
            filters: Vec::new(),
            filter_bar_focused: false,
            running: true,
            notification: None,
            terminal_size: (80, 24),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn empty_project_dashboard_navigation_does_not_panic() {
        let state = make_app_state(vec![]);
        let mut dashboard = Dashboard::new();

        // All navigation should be safe on empty project
        for _ in 0..10 {
            dashboard.handle_key(key(crossterm::event::KeyCode::Char('j')), &state);
        }
        for _ in 0..10 {
            dashboard.handle_key(key(crossterm::event::KeyCode::Char('k')), &state);
        }
        // Enter on empty should not panic
        dashboard.handle_key(key(crossterm::event::KeyCode::Enter), &state);
    }

    #[test]
    fn empty_project_n_opens_create_form() {
        let state = make_app_state(vec![]);
        let mut dashboard = Dashboard::new();

        let actions = dashboard.handle_key(key(crossterm::event::KeyCode::Char('n')), &state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::OpenCreateForm));
    }

    #[test]
    fn empty_ready_and_recent_lists() {
        let stories: Vec<StorySnapshot> = vec![];
        let ready = ready_stories(&stories);
        assert!(ready.is_empty());
        let recent = recent_stories(&stories);
        assert!(recent.is_empty());
    }

    #[test]
    fn all_blocked_means_no_ready_stories() {
        let stories = vec![
            make_snapshot(
                "SH-1",
                "todo",
                "A",
                Priority::High,
                Some("waiting for API"),
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-2",
                "in-progress",
                "B",
                Priority::Critical,
                Some("blocked by deploy"),
                "2026-01-02T00:00:00Z",
            ),
        ];
        let ready = ready_stories(&stories);
        assert!(ready.is_empty());
    }

    #[test]
    fn dashboard_view_board_action_from_keyboard() {
        let state = make_app_state(vec![make_snapshot(
            "SH-1",
            "todo",
            "A",
            Priority::None,
            None,
            "2026-01-01T00:00:00Z",
        )]);
        let mut dashboard = Dashboard::new();

        let actions = dashboard.handle_key(key(crossterm::event::KeyCode::Char('2')), &state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::SwitchView(View::Board)));
    }

    // =======================================================================
    // QA: Dashboard cursor on stories
    // =======================================================================

    #[test]
    fn dashboard_enter_on_story_opens_detail() {
        let state = make_app_state(vec![make_snapshot(
            "SH-1",
            "todo",
            "Alpha",
            Priority::High,
            None,
            "2026-01-01T00:00:00Z",
        )]);
        let mut dashboard = Dashboard::new();

        // Navigate to find a story item
        // Build items to inspect structure
        dashboard.build_items(&state);

        // Find the first selectable story index
        let story_idx = dashboard
            .items
            .iter()
            .position(|item| matches!(item, DashboardItem::Story { .. }));
        if let Some(idx) = story_idx {
            dashboard.cursor = idx;
            let actions = dashboard.handle_key(key(crossterm::event::KeyCode::Enter), &state);
            assert_eq!(actions.len(), 1);
            assert!(matches!(&actions[0], Action::OpenDetail(id) if id == "SH-1"));
        }
    }

    #[test]
    fn dashboard_on_state_change_clamps_cursor() {
        let state = make_app_state(vec![
            make_snapshot(
                "SH-1",
                "todo",
                "A",
                Priority::None,
                None,
                "2026-01-01T00:00:00Z",
            ),
            make_snapshot(
                "SH-2",
                "todo",
                "B",
                Priority::None,
                None,
                "2026-01-02T00:00:00Z",
            ),
            make_snapshot(
                "SH-3",
                "todo",
                "C",
                Priority::None,
                None,
                "2026-01-03T00:00:00Z",
            ),
        ]);
        let mut dashboard = Dashboard::new();
        dashboard.build_items(&state);

        // Set cursor to end
        dashboard.cursor = dashboard.items.len().saturating_sub(1);

        // Rebuild with fewer stories (simulating data change)
        let empty_state = make_app_state(vec![]);
        dashboard.on_state_change(&empty_state);

        // Cursor should be clamped
        if !dashboard.items.is_empty() {
            assert!(dashboard.cursor < dashboard.items.len());
        }
    }
}
