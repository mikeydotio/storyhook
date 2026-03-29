use ratatui::style::{Color, Modifier, Style};

/// Full theme for the TUI, respecting `$NO_COLOR`.
///
/// When `$NO_COLOR` is set (any non-empty value), all color is removed and
/// only Bold/Dim/Underline modifiers are used.
#[derive(Debug, Clone)]
pub struct Theme {
    pub story_id: Style,
    pub story_title: Style,
    pub priority_critical: Style,
    pub priority_high: Style,
    pub priority_medium: Style,
    pub priority_low: Style,
    pub labels: Style,
    pub assignee: Style,
    pub blocked_badge: Style,
    pub section_header: Style,
    pub section_count: Style,
    pub cursor: Style,
    pub selected_row: Style,
    pub filter_chip: Style,
    pub status_bar: Style,
    pub status_bar_keys: Style,
    pub modal_border: Style,
    pub disclosure: Style,
    pub drag_source: Style,
    pub drag_target: Style,
}

impl Theme {
    /// Build a theme, checking `$NO_COLOR` from the environment.
    pub fn from_env() -> Self {
        let no_color = std::env::var("NO_COLOR").is_ok_and(|v| !v.is_empty());
        if no_color {
            Self::no_color()
        } else {
            Self::colored()
        }
    }

    /// Construct a theme with ANSI 16 named colors.
    pub fn colored() -> Self {
        Self {
            story_id: Style::default().fg(Color::DarkGray),
            story_title: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            priority_critical: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            priority_high: Style::default().fg(Color::Yellow),
            priority_medium: Style::default().fg(Color::Blue),
            priority_low: Style::default().fg(Color::DarkGray),
            labels: Style::default().fg(Color::Cyan),
            assignee: Style::default().fg(Color::Green),
            blocked_badge: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            section_header: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            section_count: Style::default().fg(Color::DarkGray),
            cursor: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            selected_row: Style::default().bg(Color::DarkGray),
            filter_chip: Style::default().fg(Color::Black).bg(Color::Cyan),
            status_bar: Style::default().fg(Color::DarkGray),
            status_bar_keys: Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            modal_border: Style::default().fg(Color::Cyan),
            disclosure: Style::default().fg(Color::DarkGray),
            drag_source: Style::default().add_modifier(Modifier::DIM),
            drag_target: Style::default().fg(Color::Black).bg(Color::Cyan),
        }
    }

    /// Construct a theme with no color -- only Bold/Dim/Underline modifiers.
    pub fn no_color() -> Self {
        let reset = Color::Reset;
        Self {
            story_id: Style::default().fg(reset).add_modifier(Modifier::DIM),
            story_title: Style::default().fg(reset).add_modifier(Modifier::BOLD),
            priority_critical: Style::default().fg(reset).add_modifier(Modifier::BOLD),
            priority_high: Style::default().fg(reset),
            priority_medium: Style::default().fg(reset),
            priority_low: Style::default().fg(reset).add_modifier(Modifier::DIM),
            labels: Style::default().fg(reset),
            assignee: Style::default().fg(reset),
            blocked_badge: Style::default().fg(reset).add_modifier(Modifier::BOLD),
            section_header: Style::default().fg(reset).add_modifier(Modifier::BOLD),
            section_count: Style::default().fg(reset).add_modifier(Modifier::DIM),
            cursor: Style::default().fg(reset).add_modifier(Modifier::BOLD),
            selected_row: Style::default().fg(reset).add_modifier(Modifier::UNDERLINED),
            filter_chip: Style::default().fg(reset).add_modifier(Modifier::BOLD),
            status_bar: Style::default().fg(reset).add_modifier(Modifier::DIM),
            status_bar_keys: Style::default().fg(reset).add_modifier(Modifier::BOLD),
            modal_border: Style::default().fg(reset),
            disclosure: Style::default().fg(reset).add_modifier(Modifier::DIM),
            drag_source: Style::default().add_modifier(Modifier::DIM),
            drag_target: Style::default().fg(reset).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colored_theme_uses_ansi_colors() {
        let theme = Theme::colored();
        assert_eq!(theme.story_id.fg, Some(Color::DarkGray));
        assert_eq!(theme.story_title.fg, Some(Color::White));
        assert!(theme.story_title.add_modifier.contains(Modifier::BOLD));
        assert_eq!(theme.labels.fg, Some(Color::Cyan));
        assert_eq!(theme.assignee.fg, Some(Color::Green));
    }

    #[test]
    fn no_color_theme_has_no_ansi_colors() {
        let theme = Theme::no_color();
        // All fg colors should be Reset
        assert_eq!(theme.story_id.fg, Some(Color::Reset));
        assert_eq!(theme.story_title.fg, Some(Color::Reset));
        assert_eq!(theme.priority_critical.fg, Some(Color::Reset));
        assert_eq!(theme.labels.fg, Some(Color::Reset));
        assert_eq!(theme.assignee.fg, Some(Color::Reset));
        // No bg colors set (except selected_row which uses underline instead)
        assert_eq!(theme.selected_row.bg, None);
    }

    #[test]
    fn no_color_theme_uses_modifiers() {
        let theme = Theme::no_color();
        assert!(theme.story_title.add_modifier.contains(Modifier::BOLD));
        assert!(theme.story_id.add_modifier.contains(Modifier::DIM));
        assert!(theme.selected_row.add_modifier.contains(Modifier::UNDERLINED));
        assert!(theme.status_bar_keys.add_modifier.contains(Modifier::BOLD));
    }
}
