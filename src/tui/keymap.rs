use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::action::{Action, View};

/// The context in which a key event is received, determining which bindings apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyContext {
    Global,
    Board,
    FilterBarFocused,
    StoryDetail,
    CreateForm,
    Help,
}

/// Map a key event + context to an optional Action.
///
/// Returns `None` if the key has no binding in the given context.
/// Component-local navigation (j/k cursor movement on board) returns `None`
/// here -- the component handles those keys directly.
pub fn map_key(key: KeyEvent, context: KeyContext) -> Option<Action> {
    // Ctrl+C is always force quit, regardless of context
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(Action::Quit);
    }

    match context {
        KeyContext::Global => map_global(key),
        KeyContext::Board => map_board(key),
        KeyContext::FilterBarFocused => map_filter_bar(key),
        KeyContext::StoryDetail => map_story_detail(key),
        KeyContext::CreateForm => map_create_form(key),
        KeyContext::Help => map_help(key),
    }
}

fn map_global(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('?') => Some(Action::ToggleHelp),
        KeyCode::Char('1') => Some(Action::SwitchView(View::Dashboard)),
        KeyCode::Char('2') => Some(Action::SwitchView(View::Board)),
        KeyCode::Char('r') => Some(Action::RefreshData),
        _ => None,
    }
}

fn map_board(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('n') => Some(Action::OpenCreateForm),
        KeyCode::Char('/') => Some(Action::FocusFilterBar),
        // j/k/Up/Down/h/l/g/G/Space/Enter/>/</H/L are handled by the
        // Board component directly, returning actions as needed.
        // We don't map them here -- the board has the context needed
        // (cursor position, selected row type, etc.)
        _ => None,
    }
}

fn map_filter_bar(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::UnfocusFilterBar),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::ClearAllFilters)
        }
        // Text input, Enter, Tab, Backspace handled by FilterBar component
        _ => None,
    }
}

fn map_story_detail(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::CloseModal),
        // j/k/e/Enter/c/Ctrl+E/>/< handled by StoryDetail component
        _ => None,
    }
}

fn map_create_form(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::CloseModal),
        // Tab/Shift+Tab/Enter handled by CreateForm component
        _ => None,
    }
}

fn map_help(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => Some(Action::CloseModal),
        // j/k scroll handled by Help component
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn global_q_quits() {
        let action = map_key(key(KeyCode::Char('q')), KeyContext::Global);
        assert!(matches!(action, Some(Action::Quit)));
    }

    #[test]
    fn global_question_mark_toggles_help() {
        let action = map_key(key(KeyCode::Char('?')), KeyContext::Global);
        assert!(matches!(action, Some(Action::ToggleHelp)));
    }

    #[test]
    fn global_1_switches_to_dashboard() {
        let action = map_key(key(KeyCode::Char('1')), KeyContext::Global);
        assert!(matches!(action, Some(Action::SwitchView(View::Dashboard))));
    }

    #[test]
    fn global_2_switches_to_board() {
        let action = map_key(key(KeyCode::Char('2')), KeyContext::Global);
        assert!(matches!(action, Some(Action::SwitchView(View::Board))));
    }

    #[test]
    fn global_r_refreshes() {
        let action = map_key(key(KeyCode::Char('r')), KeyContext::Global);
        assert!(matches!(action, Some(Action::RefreshData)));
    }

    #[test]
    fn ctrl_c_quits_from_any_context() {
        for ctx in [
            KeyContext::Global,
            KeyContext::Board,
            KeyContext::FilterBarFocused,
            KeyContext::StoryDetail,
            KeyContext::CreateForm,
            KeyContext::Help,
        ] {
            let action = map_key(key_ctrl('c'), ctx);
            assert!(matches!(action, Some(Action::Quit)), "Ctrl+C should quit in {ctx:?}");
        }
    }

    #[test]
    fn board_n_opens_create_form() {
        let action = map_key(key(KeyCode::Char('n')), KeyContext::Board);
        assert!(matches!(action, Some(Action::OpenCreateForm)));
    }

    #[test]
    fn board_slash_focuses_filter_bar() {
        let action = map_key(key(KeyCode::Char('/')), KeyContext::Board);
        assert!(matches!(action, Some(Action::FocusFilterBar)));
    }

    #[test]
    fn filter_bar_esc_unfocuses() {
        let action = map_key(key(KeyCode::Esc), KeyContext::FilterBarFocused);
        assert!(matches!(action, Some(Action::UnfocusFilterBar)));
    }

    #[test]
    fn filter_bar_ctrl_u_clears_all() {
        let action = map_key(key_ctrl('u'), KeyContext::FilterBarFocused);
        assert!(matches!(action, Some(Action::ClearAllFilters)));
    }

    #[test]
    fn story_detail_esc_closes() {
        let action = map_key(key(KeyCode::Esc), KeyContext::StoryDetail);
        assert!(matches!(action, Some(Action::CloseModal)));
    }

    #[test]
    fn create_form_esc_closes() {
        let action = map_key(key(KeyCode::Esc), KeyContext::CreateForm);
        assert!(matches!(action, Some(Action::CloseModal)));
    }

    #[test]
    fn help_esc_closes() {
        let action = map_key(key(KeyCode::Esc), KeyContext::Help);
        assert!(matches!(action, Some(Action::CloseModal)));
    }

    #[test]
    fn help_q_closes() {
        let action = map_key(key(KeyCode::Char('q')), KeyContext::Help);
        assert!(matches!(action, Some(Action::CloseModal)));
    }

    #[test]
    fn help_question_mark_closes() {
        let action = map_key(key(KeyCode::Char('?')), KeyContext::Help);
        assert!(matches!(action, Some(Action::CloseModal)));
    }

    #[test]
    fn unmapped_key_returns_none() {
        let action = map_key(key(KeyCode::Char('z')), KeyContext::Global);
        assert!(action.is_none());
    }
}
