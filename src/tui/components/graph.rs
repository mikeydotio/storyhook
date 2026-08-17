use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::domain::{Priority, StorySnapshot, derive_family_relationships};
use crate::tui::action::Action;
use crate::tui::state::AppState;
use crate::tui::theme::Theme;

use super::{Component, HitRegion, HitTarget};

/// The four display modes for the graph view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphMode {
    Tree,
    Dependencies,
    CriticalPath,
    Focus,
}

impl GraphMode {
    fn next(self) -> Self {
        match self {
            Self::Tree => Self::Dependencies,
            Self::Dependencies => Self::CriticalPath,
            Self::CriticalPath => Self::Focus,
            Self::Focus => Self::Tree,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tree => "Tree",
            Self::Dependencies => "Dependencies",
            Self::CriticalPath => "Critical Path",
            Self::Focus => "Focus",
        }
    }
}

/// A single renderable line in the graph view.
#[derive(Debug, Clone)]
enum GraphLine {
    StoryNode {
        id: String,
        title: String,
        state: String,
        priority: Priority,
        connector: String,
    },
    RelationRow {
        from_id: String,
        relation: String,
        to_id: String,
    },
    SectionHeader {
        text: String,
    },
    Empty,
}

impl GraphLine {
    /// Return the story ID if this line represents a story.
    fn story_id(&self) -> Option<&str> {
        match self {
            Self::StoryNode { id, .. } => Some(id),
            Self::RelationRow { from_id, .. } => Some(from_id),
            _ => None,
        }
    }
}

/// Dependency graph view component.
pub struct GraphComponent {
    mode: GraphMode,
    cursor: usize,
    scroll_offset: usize,
    focused_story: Option<String>,
    cached_lines: Vec<GraphLine>,
    hit_regions: Vec<HitRegion>,
    render_area: Rect,
}

impl Default for GraphComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphComponent {
    pub fn new() -> Self {
        Self {
            mode: GraphMode::Tree,
            cursor: 0,
            scroll_offset: 0,
            focused_story: None,
            cached_lines: Vec::new(),
            hit_regions: Vec::new(),
            render_area: Rect::default(),
        }
    }

    /// Access the current mode (for testing).
    pub fn mode(&self) -> GraphMode {
        self.mode
    }

    /// Access the current cursor position (for testing).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Build display lines from current stories and mode.
    fn build_lines(&mut self, state: &AppState) {
        self.cached_lines = match self.mode {
            GraphMode::Tree => self.build_tree_lines(state),
            GraphMode::Dependencies => self.build_deps_lines(state),
            GraphMode::CriticalPath => self.build_critical_path_lines(state),
            GraphMode::Focus => self.build_focus_lines(state),
        };
        self.clamp_cursor();
    }

    fn build_tree_lines(&self, state: &AppState) -> Vec<GraphLine> {
        let stories = stories_map(&state.data.stories);
        if stories.is_empty() {
            return vec![GraphLine::SectionHeader {
                text: "Tree View".to_string(),
            }];
        }

        let mut lines = vec![GraphLine::SectionHeader {
            text: "Tree View".to_string(),
        }];

        // Find parent-child relationships
        let mut children_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut has_parent: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        for story in stories.values() {
            for rel in &story.relationships {
                match rel.relation.as_str() {
                    "parent-of" if stories.contains_key(&rel.other_id) => {
                        children_map
                            .entry(story.id.clone())
                            .or_default()
                            .push(rel.other_id.clone());
                        has_parent.insert(rel.other_id.clone());
                    }
                    "child-of" if stories.contains_key(&rel.other_id) => {
                        children_map
                            .entry(rel.other_id.clone())
                            .or_default()
                            .push(story.id.clone());
                        has_parent.insert(story.id.clone());
                    }
                    _ => {}
                }
            }
        }

        // Sort children for deterministic output
        for children in children_map.values_mut() {
            children.sort();
            children.dedup();
        }

        // Root stories: those with no parent
        let mut roots: Vec<&String> = stories
            .keys()
            .filter(|id| !has_parent.contains(*id))
            .collect();
        roots.sort();

        for root_id in &roots {
            self.render_tree_node(
                root_id,
                &stories,
                &children_map,
                &mut lines,
                String::new(),
                true,
            );
        }

        lines
    }

    fn render_tree_node(
        &self,
        id: &str,
        stories: &BTreeMap<String, &StorySnapshot>,
        children_map: &BTreeMap<String, Vec<String>>,
        lines: &mut Vec<GraphLine>,
        prefix: String,
        is_last: bool,
    ) {
        if let Some(story) = stories.get(id) {
            let connector = if prefix.is_empty() {
                "  ".to_string()
            } else if is_last {
                format!("{prefix}\u{2514}\u{2500} ")
            } else {
                format!("{prefix}\u{251c}\u{2500} ")
            };

            lines.push(GraphLine::StoryNode {
                id: story.id.clone(),
                title: story.title.clone(),
                state: story.state.clone(),
                priority: story.priority.clone(),
                connector,
            });

            if let Some(children) = children_map.get(id) {
                let child_prefix = if prefix.is_empty() {
                    "  ".to_string()
                } else if is_last {
                    format!("{prefix}   ")
                } else {
                    format!("{prefix}\u{2502}  ")
                };
                for (i, child_id) in children.iter().enumerate() {
                    let child_is_last = i == children.len() - 1;
                    self.render_tree_node(
                        child_id,
                        stories,
                        children_map,
                        lines,
                        child_prefix.clone(),
                        child_is_last,
                    );
                }
            }
        }
    }

    fn build_deps_lines(&self, state: &AppState) -> Vec<GraphLine> {
        let stories = stories_map(&state.data.stories);
        let mut lines = vec![GraphLine::SectionHeader {
            text: "Dependencies".to_string(),
        }];

        let mut has_relations = false;
        for story in stories.values() {
            for rel in &story.relationships {
                if stories.contains_key(&rel.other_id) {
                    lines.push(GraphLine::RelationRow {
                        from_id: story.id.clone(),
                        relation: rel.relation.clone(),
                        to_id: rel.other_id.clone(),
                    });
                    has_relations = true;
                }
            }
        }

        if !has_relations {
            lines.push(GraphLine::Empty);
        }

        lines
    }

    fn build_critical_path_lines(&self, state: &AppState) -> Vec<GraphLine> {
        let stories = stories_map(&state.data.stories);
        let mut lines = vec![GraphLine::SectionHeader {
            text: "Critical Path".to_string(),
        }];

        // Build a "blocks"/"blocked-by" chain graph, then find longest path
        // We look for "blocks", "blocked-by", and parent-of/child-of relationships
        let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for story in stories.values() {
            for rel in &story.relationships {
                if !stories.contains_key(&rel.other_id) {
                    continue;
                }
                match rel.relation.as_str() {
                    "blocks" => {
                        // story blocks rel.other_id => story must come before other_id
                        adjacency
                            .entry(story.id.clone())
                            .or_default()
                            .push(rel.other_id.clone());
                    }
                    "blocked-by" => {
                        // story blocked-by rel.other_id => other_id must come before story
                        adjacency
                            .entry(rel.other_id.clone())
                            .or_default()
                            .push(story.id.clone());
                    }
                    "parent-of" => {
                        adjacency
                            .entry(story.id.clone())
                            .or_default()
                            .push(rel.other_id.clone());
                    }
                    "child-of" => {
                        adjacency
                            .entry(rel.other_id.clone())
                            .or_default()
                            .push(story.id.clone());
                    }
                    _ => {}
                }
            }
        }

        if adjacency.is_empty() {
            lines.push(GraphLine::SectionHeader {
                text: "  No dependency chains found.".to_string(),
            });
            return lines;
        }

        // Find longest path via DFS from each node
        let all_ids: Vec<String> = stories.keys().cloned().collect();
        let mut longest: Vec<String> = Vec::new();

        for start_id in &all_ids {
            let path = longest_path_from(start_id, &adjacency);
            if path.len() > longest.len() {
                longest = path;
            }
        }

        if longest.len() <= 1 {
            lines.push(GraphLine::SectionHeader {
                text: "  No dependency chains found.".to_string(),
            });
            return lines;
        }

        for (i, id) in longest.iter().enumerate() {
            if let Some(story) = stories.get(id) {
                // Use a simpler connector for rendering
                let display_connector = if i == 0 {
                    "  ".to_string()
                } else {
                    "  \u{2514}\u{2500} ".to_string()
                };
                lines.push(GraphLine::StoryNode {
                    id: story.id.clone(),
                    title: story.title.clone(),
                    state: story.state.clone(),
                    priority: story.priority.clone(),
                    connector: display_connector,
                });
                // Add a vertical connector between items (except after last)
                if i < longest.len() - 1 {
                    lines.push(GraphLine::SectionHeader {
                        text: "  \u{2502}".to_string(),
                    });
                }
            }
        }

        lines
    }

    fn build_focus_lines(&self, state: &AppState) -> Vec<GraphLine> {
        let stories = stories_map(&state.data.stories);
        let focused_id = match &self.focused_story {
            Some(id) if stories.contains_key(id) => id.clone(),
            _ => {
                let mut lines = vec![GraphLine::SectionHeader {
                    text: "Focus".to_string(),
                }];
                lines.push(GraphLine::SectionHeader {
                    text: "  Press Enter on a story to focus it.".to_string(),
                });
                return lines;
            }
        };

        let story = stories[&focused_id];
        let mut lines = vec![GraphLine::SectionHeader {
            text: format!("Focus: {} \u{2014} {}", story.id, story.title),
        }];

        // Direct relationships
        let direct_rels: Vec<_> = story.relationships.clone();
        if !direct_rels.is_empty() {
            lines.push(GraphLine::SectionHeader {
                text: "  Direct:".to_string(),
            });
            for rel in &direct_rels {
                if stories.contains_key(&rel.other_id) {
                    lines.push(GraphLine::RelationRow {
                        from_id: story.id.clone(),
                        relation: rel.relation.clone(),
                        to_id: rel.other_id.clone(),
                    });
                }
            }
        }

        // Transitive relationships from derive_family_relationships
        let btree_stories = stories_btree_map(&state.data.stories);
        let derived = derive_family_relationships(&btree_stories);
        if let Some(transitive) = derived.get(&focused_id)
            && !transitive.is_empty()
        {
            lines.push(GraphLine::SectionHeader {
                text: "  Transitive:".to_string(),
            });
            for rel in transitive {
                lines.push(GraphLine::RelationRow {
                    from_id: focused_id.clone(),
                    relation: rel.relation.clone(),
                    to_id: rel.other_id.clone(),
                });
            }
        }

        if lines.len() == 1 {
            lines.push(GraphLine::SectionHeader {
                text: "  No relationships found for this story.".to_string(),
            });
        }

        lines
    }

    fn clamp_cursor(&mut self) {
        let len = self.cached_lines.len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }

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

    /// Render a single GraphLine into a ratatui Line.
    fn render_line(line: &GraphLine, is_selected: bool, theme: &Theme) -> Line<'static> {
        let cursor_marker = if is_selected { "> " } else { "  " };
        let cursor_style = if is_selected {
            theme.cursor
        } else {
            ratatui::style::Style::default()
        };

        match line {
            GraphLine::StoryNode {
                id,
                title,
                state,
                priority,
                connector,
            } => {
                let priority_style = match priority {
                    Priority::Critical => theme.priority_critical,
                    Priority::High => theme.priority_high,
                    Priority::Medium => theme.priority_medium,
                    Priority::Low => theme.priority_low,
                    Priority::None => theme.story_id,
                };
                let state_display = format!("[{state}]");
                let row_style = if is_selected {
                    theme.selected_row
                } else {
                    ratatui::style::Style::default()
                };
                Line::from(vec![
                    Span::styled(cursor_marker.to_string(), cursor_style),
                    Span::styled(connector.clone(), row_style),
                    Span::styled(format!("{id} "), priority_style),
                    Span::styled(format!("{state_display} "), theme.story_id),
                    Span::styled(title.clone(), theme.story_title),
                ])
            }
            GraphLine::RelationRow {
                from_id,
                relation,
                to_id,
            } => {
                let row_style = if is_selected {
                    theme.selected_row
                } else {
                    ratatui::style::Style::default()
                };
                Line::from(vec![
                    Span::styled(cursor_marker.to_string(), cursor_style),
                    Span::styled(format!("    {from_id} "), theme.story_id),
                    Span::styled(
                        format!("\u{2500}\u{2500}{relation}\u{2500}\u{2500}> "),
                        row_style,
                    ),
                    Span::styled(to_id.clone(), theme.story_id),
                ])
            }
            GraphLine::SectionHeader { text } => Line::from(vec![
                Span::styled(cursor_marker.to_string(), cursor_style),
                Span::styled(text.clone(), theme.section_header),
            ]),
            GraphLine::Empty => Line::from(Span::styled(
                "  No relationships found. Stories need parent-of, blocks, or other relationships to appear here.",
                theme.story_id,
            )),
        }
    }
}

impl Component for GraphComponent {
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.cached_lines.is_empty() {
                    self.cursor = (self.cursor + 1).min(self.cached_lines.len() - 1);
                }
                vec![]
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                vec![]
            }
            KeyCode::Char('g') => {
                self.cursor = 0;
                vec![]
            }
            KeyCode::Char('G') => {
                if !self.cached_lines.is_empty() {
                    self.cursor = self.cached_lines.len() - 1;
                }
                vec![]
            }
            KeyCode::Tab | KeyCode::Char('m') => {
                self.mode = self.mode.next();
                self.cursor = 0;
                self.scroll_offset = 0;
                self.build_lines(state);
                vec![]
            }
            KeyCode::Enter => {
                if let Some(line) = self.cached_lines.get(self.cursor)
                    && let Some(id) = line.story_id()
                {
                    let id = id.to_string();
                    if self.mode == GraphMode::Focus {
                        self.focused_story = Some(id.clone());
                        self.build_lines(state);
                        return vec![];
                    }
                    return vec![Action::OpenDetail(id)];
                }
                vec![]
            }
            KeyCode::Esc => {
                if self.mode == GraphMode::Focus {
                    self.mode = GraphMode::Tree;
                    self.focused_story = None;
                    self.cursor = 0;
                    self.scroll_offset = 0;
                    self.build_lines(state);
                }
                vec![]
            }
            KeyCode::Char('?') => vec![Action::ToggleHelp],
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, _state: &AppState) -> Vec<Action> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let row = mouse.row.saturating_sub(self.render_area.y) as usize;
                let index = row + self.scroll_offset;
                if index < self.cached_lines.len() {
                    self.cursor = index;
                }
                vec![]
            }
            MouseEventKind::ScrollDown => {
                if !self.cached_lines.is_empty() {
                    self.cursor = (self.cursor + 3).min(self.cached_lines.len() - 1);
                }
                vec![]
            }
            MouseEventKind::ScrollUp => {
                self.cursor = self.cursor.saturating_sub(3);
                vec![]
            }
            _ => vec![],
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        self.render_area = area;

        // Rebuild lines on every render to stay current
        self.build_lines(state);

        let theme = Theme::from_env();

        if self.cached_lines.is_empty() {
            let msg = Line::from(Span::styled("No stories found.", theme.story_id));
            frame.render_widget(Paragraph::new(vec![msg]), area);
            return;
        }

        let viewport_height = area.height as usize;
        self.update_scroll(viewport_height);

        // Build hit regions and visible lines
        self.hit_regions.clear();
        let visible: Vec<Line> = self
            .cached_lines
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(viewport_height)
            .enumerate()
            .map(|(row_in_viewport, (line_idx, graph_line))| {
                let is_selected = line_idx == self.cursor;

                // Register hit region for story lines
                if let Some(id) = graph_line.story_id() {
                    self.hit_regions.push(HitRegion {
                        rect: Rect {
                            x: area.x,
                            y: area.y + row_in_viewport as u16,
                            width: area.width,
                            height: 1,
                        },
                        target: HitTarget::StoryRow { id: id.to_string() },
                    });
                }

                Self::render_line(graph_line, is_selected, &theme)
            })
            .collect();

        // Mode indicator at top-right
        let mode_label = format!("[{}]", self.mode.label());
        let mode_x = area.x + area.width.saturating_sub(mode_label.len() as u16 + 1);
        let mode_span = Span::styled(mode_label, theme.section_count);
        frame.render_widget(
            Paragraph::new(Line::from(mode_span)),
            Rect {
                x: mode_x,
                y: area.y,
                width: area.width.saturating_sub(mode_x - area.x),
                height: 1,
            },
        );

        frame.render_widget(Paragraph::new(visible), area);
    }

    fn on_state_change(&mut self, state: &AppState) {
        self.build_lines(state);
    }

    fn hit_regions(&self) -> &[HitRegion] {
        &self.hit_regions
    }
}

/// Build a BTreeMap<String, &StorySnapshot> for quick lookup by ID.
fn stories_map(stories: &[StorySnapshot]) -> BTreeMap<String, &StorySnapshot> {
    stories.iter().map(|s| (s.id.clone(), s)).collect()
}

/// Build a BTreeMap<String, StorySnapshot> for domain functions that need owned snapshots.
fn stories_btree_map(stories: &[StorySnapshot]) -> BTreeMap<String, StorySnapshot> {
    stories.iter().map(|s| (s.id.clone(), s.clone())).collect()
}

/// Find the longest path starting from `start` in a DAG via DFS.
fn longest_path_from(start: &str, adjacency: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut best = vec![start.to_string()];
    let mut stack: Vec<(String, Vec<String>)> = vec![(start.to_string(), vec![start.to_string()])];

    while let Some((current, path)) = stack.pop() {
        if let Some(neighbors) = adjacency.get(&current) {
            for next in neighbors {
                if !path.contains(next) {
                    let mut new_path = path.clone();
                    new_path.push(next.clone());
                    if new_path.len() > best.len() {
                        best = new_path.clone();
                    }
                    stack.push((next.clone(), new_path));
                }
            }
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Priority, StoryRelation, StorySnapshot, SuperState};
    use crate::tui::action::{Action, View};
    use crate::tui::data::DataStore;
    use crate::tui::focus::{FocusStack, FocusTarget};
    use crate::tui::state::AppState;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn test_states() -> Vec<crate::domain::StateDef> {
        vec![
            crate::domain::StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: None,
            },
            crate::domain::StateDef {
                slug: "in-progress".to_string(),
                super_state: SuperState::Open,
                role: Some("active".to_string()),
                description: None,
            },
        ]
    }

    fn snapshot(id: &str, title: &str, state: &str, rels: Vec<StoryRelation>) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: title.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: state.to_string(),
            superstate: SuperState::Open,
            assignee: None,
            awaiting: None,
            comments: vec![],
            referenced_by_commits: vec![],
            relationships: rels,
            priority: Priority::None,
            priority_assessed: false,
            labels: vec![],
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

    fn make_state(stories: Vec<StorySnapshot>) -> AppState {
        AppState {
            data: DataStore::from_test_data(test_states(), stories, "SH".to_string(), vec![]),
            focus: FocusStack::new(FocusTarget::Graph),
            view: View::Graph,
            filters: Vec::new(),
            filter_bar_focused: false,
            running: true,
            notification: None,
            terminal_size: (80, 24),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    fn rel(relation: &str, other_id: &str) -> StoryRelation {
        StoryRelation {
            relation: relation.to_string(),
            other_id: other_id.to_string(),
        }
    }

    // 1. Tree mode renders hierarchy
    #[test]
    fn tree_mode_renders_hierarchy() {
        let stories = vec![
            snapshot(
                "SH-1",
                "Parent story",
                "todo",
                vec![rel("parent-of", "SH-2")],
            ),
            snapshot("SH-2", "Child story", "todo", vec![]),
            snapshot("SH-3", "Orphan story", "todo", vec![]),
        ];
        let state = make_state(stories);
        let mut graph = GraphComponent::new();
        graph.build_lines(&state);

        // Should have header + 3 story nodes
        let story_nodes: Vec<_> = graph
            .cached_lines
            .iter()
            .filter(|l| matches!(l, GraphLine::StoryNode { .. }))
            .collect();
        assert_eq!(story_nodes.len(), 3, "Should have 3 story nodes in tree");

        // SH-1 should come before SH-2 (parent before child)
        let ids: Vec<&str> = graph
            .cached_lines
            .iter()
            .filter_map(|l| match l {
                GraphLine::StoryNode { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let pos_1 = ids.iter().position(|id| *id == "SH-1").unwrap();
        let pos_2 = ids.iter().position(|id| *id == "SH-2").unwrap();
        assert!(pos_1 < pos_2, "Parent SH-1 should appear before child SH-2");

        // SH-2 should have an indented connector (contains box-drawing chars)
        if let GraphLine::StoryNode { connector, .. } = &graph.cached_lines[pos_2 + 1] {
            // +1 for header line
            assert!(
                connector.contains('\u{2514}') || connector.contains('\u{251c}'),
                "Child connector should use box-drawing characters"
            );
        }
    }

    // 2. Dependencies mode shows all relations
    #[test]
    fn dependencies_mode_shows_all_relations() {
        let stories = vec![
            snapshot(
                "SH-1",
                "Story 1",
                "todo",
                vec![rel("parent-of", "SH-2"), rel("blocks", "SH-3")],
            ),
            snapshot("SH-2", "Story 2", "todo", vec![]),
            snapshot("SH-3", "Story 3", "todo", vec![rel("blocked-by", "SH-1")]),
        ];
        let state = make_state(stories);
        let mut graph = GraphComponent::new();
        graph.mode = GraphMode::Dependencies;
        graph.build_lines(&state);

        let relation_rows: Vec<_> = graph
            .cached_lines
            .iter()
            .filter(|l| matches!(l, GraphLine::RelationRow { .. }))
            .collect();
        // SH-1 parent-of SH-2, SH-1 blocks SH-3, SH-3 blocked-by SH-1
        assert_eq!(relation_rows.len(), 3, "Should have 3 relationship rows");
    }

    // 3. Focus mode shows transitive
    #[test]
    fn focus_mode_shows_transitive() {
        // SH-1 parent-of SH-2, SH-2 parent-of SH-3
        // So SH-1 is ancestor-of SH-3 (transitive)
        let stories = vec![
            snapshot("SH-1", "Root", "todo", vec![rel("parent-of", "SH-2")]),
            snapshot("SH-2", "Middle", "todo", vec![rel("parent-of", "SH-3")]),
            snapshot("SH-3", "Leaf", "todo", vec![]),
        ];
        let state = make_state(stories);
        let mut graph = GraphComponent::new();
        graph.mode = GraphMode::Focus;
        graph.focused_story = Some("SH-1".to_string());
        graph.build_lines(&state);

        // Should contain direct relations (parent-of SH-2) and transitive (ancestor-of SH-3)
        let relation_rows: Vec<_> = graph
            .cached_lines
            .iter()
            .filter_map(|l| match l {
                GraphLine::RelationRow {
                    relation, to_id, ..
                } => Some((relation.as_str(), to_id.as_str())),
                _ => None,
            })
            .collect();

        assert!(
            relation_rows
                .iter()
                .any(|(r, t)| *r == "parent-of" && *t == "SH-2"),
            "Should show direct parent-of SH-2"
        );
        assert!(
            relation_rows
                .iter()
                .any(|(r, t)| *r == "ancestor-of" && *t == "SH-3"),
            "Should show transitive ancestor-of SH-3"
        );
    }

    // 4. Mode cycling
    #[test]
    fn mode_cycling() {
        let state = make_state(vec![]);
        let mut graph = GraphComponent::new();
        assert_eq!(graph.mode, GraphMode::Tree);

        graph.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(graph.mode, GraphMode::Dependencies);

        graph.handle_key(key(KeyCode::Char('m')), &state);
        assert_eq!(graph.mode, GraphMode::CriticalPath);

        graph.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(graph.mode, GraphMode::Focus);

        graph.handle_key(key(KeyCode::Tab), &state);
        assert_eq!(graph.mode, GraphMode::Tree);
    }

    // 5. Cursor navigation
    #[test]
    fn cursor_navigation() {
        let stories = vec![
            snapshot("SH-1", "Story 1", "todo", vec![]),
            snapshot("SH-2", "Story 2", "todo", vec![]),
            snapshot("SH-3", "Story 3", "todo", vec![]),
        ];
        let state = make_state(stories);
        let mut graph = GraphComponent::new();
        graph.build_lines(&state);

        assert_eq!(graph.cursor, 0);

        // j moves down
        graph.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(graph.cursor, 1);

        graph.handle_key(key(KeyCode::Char('j')), &state);
        assert_eq!(graph.cursor, 2);

        // k moves up
        graph.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(graph.cursor, 1);

        // k at 0 stays at 0
        graph.handle_key(key(KeyCode::Char('k')), &state);
        graph.handle_key(key(KeyCode::Char('k')), &state);
        assert_eq!(graph.cursor, 0);

        // G jumps to last
        graph.handle_key(key(KeyCode::Char('G')), &state);
        assert_eq!(graph.cursor, graph.cached_lines.len() - 1);

        // g jumps to first
        graph.handle_key(key(KeyCode::Char('g')), &state);
        assert_eq!(graph.cursor, 0);
    }

    // 6. Empty graph no relationships
    #[test]
    fn empty_graph_no_relationships() {
        let stories = vec![
            snapshot("SH-1", "Story 1", "todo", vec![]),
            snapshot("SH-2", "Story 2", "todo", vec![]),
        ];
        let state = make_state(stories);
        let mut graph = GraphComponent::new();
        graph.mode = GraphMode::Dependencies;
        graph.build_lines(&state);

        // Should have an Empty line since no relationships exist
        let has_empty = graph
            .cached_lines
            .iter()
            .any(|l| matches!(l, GraphLine::Empty));
        assert!(
            has_empty,
            "Dependencies mode with no relationships should show empty message"
        );
    }

    // 7. Enter on story emits OpenDetail
    #[test]
    fn enter_on_story_emits_open_detail() {
        let stories = vec![
            snapshot("SH-1", "Story 1", "todo", vec![]),
            snapshot("SH-2", "Story 2", "todo", vec![]),
        ];
        let state = make_state(stories);
        let mut graph = GraphComponent::new();
        graph.build_lines(&state);

        // Move cursor to a story node (skip header at index 0)
        graph.handle_key(key(KeyCode::Char('j')), &state);
        let actions = graph.handle_key(key(KeyCode::Enter), &state);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::OpenDetail(id) => assert_eq!(id, "SH-1"),
            other => panic!("Expected OpenDetail, got {other:?}"),
        }
    }
}
