use std::path::Path;
use std::time::Instant;

use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::Frame;

use crate::domain::SuperState;
use crate::error::AppError;

use super::action::{Action, View};
use super::components::board::Board;
use super::components::create_form::CreateForm;
use super::components::dashboard::Dashboard;
use super::components::filter_bar::FilterBar;
use super::components::help::Help;
use super::components::status_bar::StatusBar;
use super::components::story_detail::StoryDetail;
use super::components::Component;
use super::data::DataStore;
use super::event::{Event, EventSource};
use super::focus::{FocusTarget, Modal};
use super::keymap::{self, KeyContext};
use super::state::AppState;
use super::terminal;
use super::theme::Theme;

/// Run the TUI application.
pub fn run(root: &Path) -> Result<(), AppError> {
    let data = DataStore::load(root)?;
    let mut state = AppState::new(data);
    let theme = Theme::from_env();

    let mut term = terminal::init()?;
    let (event_source, rx) = EventSource::new(root);

    // Get initial terminal size
    if let Ok(size) = term.size() {
        state.terminal_size = (size.width, size.height);
    }

    // Create components
    let mut board = Board::new();
    let mut filter_bar = FilterBar::new();
    let mut dashboard = Dashboard::new();
    let mut status_bar = StatusBar::new();
    let mut modal_components = ModalComponents::default();

    let result = main_loop(
        &mut term,
        &mut state,
        &rx,
        root,
        &theme,
        &mut board,
        &mut filter_bar,
        &mut dashboard,
        &mut status_bar,
        &mut modal_components,
    );

    event_source.stop();
    terminal::restore();
    result
}

/// Holds optional modal component instances.
#[derive(Default)]
struct ModalComponents {
    story_detail: Option<StoryDetail>,
    create_form: Option<CreateForm>,
    help: Option<Help>,
    /// The rect of the top modal from the last render, for click-outside detection.
    modal_rect: Option<Rect>,
}

#[allow(clippy::too_many_arguments)]
fn main_loop(
    term: &mut ratatui::DefaultTerminal,
    state: &mut AppState,
    rx: &std::sync::mpsc::Receiver<Event>,
    root: &Path,
    theme: &Theme,
    board: &mut Board,
    filter_bar: &mut FilterBar,
    dashboard: &mut Dashboard,
    status_bar: &mut StatusBar,
    modal_components: &mut ModalComponents,
) -> Result<(), AppError> {
    // Initial render
    term.draw(|frame| {
        render(
            frame,
            state,
            theme,
            board,
            filter_bar,
            dashboard,
            status_bar,
            modal_components,
        )
    })?;

    while state.running {
        let event = match rx.recv() {
            Ok(e) => e,
            Err(_) => break, // All senders dropped
        };

        // Update terminal size on resize events
        if let Event::Resize(w, h) = &event {
            state.terminal_size = (*w, *h);
        }

        let actions = route_event(event, state, board, filter_bar, dashboard, modal_components);

        for action in actions {
            dispatch(action, state, root, term, board, modal_components)?;
        }

        // Expire notifications (3s timeout)
        if let Some((_, created_at)) = &state.notification
            && created_at.elapsed().as_secs() >= 3
        {
            state.notification = None;
        }

        term.draw(|frame| {
            render(
                frame,
                state,
                theme,
                board,
                filter_bar,
                dashboard,
                status_bar,
                modal_components,
            )
        })?;
    }

    Ok(())
}

/// Route an event to zero or more actions based on current focus context.
fn route_event(
    event: Event,
    state: &AppState,
    board: &mut Board,
    filter_bar: &mut FilterBar,
    dashboard: &mut Dashboard,
    modal_components: &mut ModalComponents,
) -> Vec<Action> {
    match event {
        Event::Key(key) => {
            let context = determine_key_context(state);

            // First try the keymap for this context
            match keymap::map_key(key, context) {
                Some(action) => vec![action],
                None => {
                    // For modal contexts, delegate to the modal component
                    match context {
                        KeyContext::FilterBarFocused => {
                            // Delegate to filter bar component for text input, Enter, etc.
                            filter_bar.handle_key(key, state)
                        }
                        KeyContext::StoryDetail => {
                            if let Some(ref mut detail) = modal_components.story_detail {
                                return detail.handle_key(key, state);
                            }
                            vec![]
                        }
                        KeyContext::CreateForm => {
                            if let Some(ref mut form) = modal_components.create_form {
                                return form.handle_key(key, state);
                            }
                            vec![]
                        }
                        KeyContext::Help => {
                            if let Some(ref mut help) = modal_components.help {
                                return help.handle_key(key, state);
                            }
                            vec![]
                        }
                        KeyContext::Global => match state.view {
                            View::Board => {
                                // First check keymap-level board bindings (n, /)
                                if let Some(action) = keymap::map_key(key, KeyContext::Board) {
                                    return vec![action];
                                }
                                // Then delegate to board component for navigation keys
                                board.handle_key(key, state)
                            }
                            View::Dashboard => {
                                // Delegate to dashboard component for j/k/Enter/n
                                dashboard.handle_key(key, state)
                            }
                        },
                        _ => vec![],
                    }
                }
            }
        }
        Event::Mouse(mouse) => {
            // If a modal is open, check if click is inside or outside
            if state.focus.has_modal() {
                if let MouseEventKind::Down(MouseButton::Left) = mouse.kind
                    && let Some(modal_rect) = modal_components.modal_rect
                {
                    let inside = mouse.column >= modal_rect.x
                        && mouse.column < modal_rect.x + modal_rect.width
                        && mouse.row >= modal_rect.y
                        && mouse.row < modal_rect.y + modal_rect.height;
                    if !inside {
                        return vec![Action::CloseModal];
                    }
                }
                // Route to the modal component (currently modals don't handle mouse)
                return vec![];
            }

            // No modal open: route to the active view
            match state.view {
                View::Board => {
                    // First check filter bar
                    let filter_actions = filter_bar.handle_mouse(mouse, state);
                    if !filter_actions.is_empty() {
                        return filter_actions;
                    }
                    // Then check board
                    board.handle_mouse(mouse, state)
                }
                View::Dashboard => {
                    dashboard.handle_mouse(mouse, state)
                }
            }
        }
        Event::Resize(_w, _h) => {
            // Terminal size is updated in the main loop before routing.
            // Components re-render automatically on the next draw cycle.
            vec![]
        }
        Event::DataChanged => {
            vec![Action::RefreshData]
        }
        Event::Tick => {
            // Tick drives notification expiry; no action needed
            vec![]
        }
    }
}

/// Determine the key context based on current focus state.
fn determine_key_context(state: &AppState) -> KeyContext {
    // Modals ALWAYS capture input first (focus trapping).
    // This must be checked before the filter bar to prevent a
    // stale `filter_bar_focused` flag from stealing keystrokes
    // when a modal is open on top.
    if let Some(modal) = state.focus.top_modal() {
        return match modal {
            Modal::StoryDetail { .. } => KeyContext::StoryDetail,
            Modal::CreateForm => KeyContext::CreateForm,
            Modal::Help => KeyContext::Help,
        };
    }

    // If filter bar is focused (only relevant on board view), it gets priority
    if state.filter_bar_focused && state.view == View::Board {
        return KeyContext::FilterBarFocused;
    }

    // No modal, no focused filter bar: global context (view-specific bindings checked in route_event)
    KeyContext::Global
}

/// Dispatch a single action, mutating AppState.
#[allow(clippy::too_many_arguments)]
fn dispatch(
    action: Action,
    state: &mut AppState,
    root: &Path,
    term: &mut ratatui::DefaultTerminal,
    board: &mut Board,
    modal_components: &mut ModalComponents,
) -> Result<(), AppError> {
    match action {
        Action::Quit => {
            state.running = false;
        }

        Action::SwitchView(view) => {
            // Unfocus filter bar when leaving board view
            if view != View::Board {
                state.filter_bar_focused = false;
            }
            state.view = view.clone();
            state.focus.base = match view {
                View::Dashboard => FocusTarget::Dashboard,
                View::Board => FocusTarget::Board,
            };
        }

        Action::ToggleHelp => {
            if let Some(Modal::Help) = state.focus.top_modal() {
                state.focus.pop_modal();
                modal_components.help = None;
            } else {
                modal_components.help = Some(Help::new());
                state.focus.push_modal(Modal::Help);
            }
        }

        Action::OpenDetail(id) => {
            modal_components.story_detail = Some(StoryDetail::new(id.clone()));
            state
                .focus
                .push_modal(Modal::StoryDetail { story_id: id });
        }

        Action::OpenCreateForm => {
            modal_components.create_form = Some(CreateForm::new());
            state.focus.push_modal(Modal::CreateForm);
        }

        Action::CloseModal => {
            if let Some(modal) = state.focus.pop_modal() {
                match modal {
                    Modal::StoryDetail { .. } => {
                        modal_components.story_detail = None;
                    }
                    Modal::CreateForm => {
                        modal_components.create_form = None;
                    }
                    Modal::Help => {
                        modal_components.help = None;
                    }
                }
            }
        }

        Action::FocusFilterBar => {
            state.filter_bar_focused = true;
        }

        Action::UnfocusFilterBar => {
            state.filter_bar_focused = false;
        }

        Action::RefreshData => {
            match DataStore::load(root) {
                Ok(data) => {
                    state.data = data;
                    // Notify board of state change so it can reclamp cursor
                    board.on_state_change(state);
                    // Stale modal protection: if a detail modal is open, check
                    // that the story still exists
                    if let Some(Modal::StoryDetail { story_id }) = state.focus.top_modal()
                        && state.data.find_story(story_id).is_none()
                    {
                        let id = story_id.clone();
                        state.focus.pop_modal();
                        modal_components.story_detail = None;
                        state.notification = Some((
                            format!("Story {id} no longer open"),
                            Instant::now(),
                        ));
                    }
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Refresh failed: {e}"), Instant::now()));
                }
            }
        }

        Action::Notify(msg) => {
            state.notification = Some((msg, Instant::now()));
        }

        Action::ToggleSection(_slug) => {
            // Section toggling is handled directly by the Board component
            // via Space key in handle_key. This action variant exists for
            // potential future use (e.g., mouse clicks on headers).
        }

        Action::SetFilter(spec) => {
            state.filters.push(spec);
            board.on_state_change(state);
        }

        Action::ClearFilter(index) => {
            if index < state.filters.len() {
                state.filters.remove(index);
            }
            board.on_state_change(state);
        }

        Action::ClearAllFilters => {
            state.filters.clear();
            state.filter_bar_focused = false;
            board.on_state_change(state);
        }

        Action::OpenEditor { id } => {
            // $EDITOR integration: suspend TUI, run editor, resume TUI
            let editor = std::env::var("EDITOR")
                .or_else(|_| std::env::var("VISUAL"))
                .ok();

            if let Some(editor_cmd) = editor {
                // Create temp file with placeholder text
                let tmp_dir = std::env::temp_dir();
                let tmp_path = tmp_dir.join(format!("storyhook-comment-{}.txt", &id));
                if let Err(e) = std::fs::write(&tmp_path, "") {
                    state.notification =
                        Some((format!("Failed to create temp file: {e}"), Instant::now()));
                } else {
                    // Suspend TUI
                    terminal::restore();

                    // Run editor
                    let result = std::process::Command::new(&editor_cmd)
                        .arg(&tmp_path)
                        .status();

                    // Re-init TUI
                    match terminal::init() {
                        Ok(new_term) => {
                            *term = new_term;
                        }
                        Err(e) => {
                            // Fatal: can't restore TUI
                            eprintln!("Failed to re-init terminal: {e}");
                            state.running = false;
                        }
                    }

                    match result {
                        Ok(status) if status.success() => {
                            // Read the temp file
                            match std::fs::read_to_string(&tmp_path) {
                                Ok(content) => {
                                    let text = content.trim().to_string();
                                    if !text.is_empty() {
                                        // Dispatch AddComment
                                        dispatch(
                                            Action::AddComment {
                                                id: id.clone(),
                                                text,
                                            },
                                            state,
                                            root,
                                            term,
                                            board,
                                            modal_components,
                                        )?;
                                    }
                                }
                                Err(e) => {
                                    state.notification = Some((
                                        format!("Failed to read temp file: {e}"),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }
                        Ok(_) => {
                            state.notification =
                                Some(("Editor exited with error".to_string(), Instant::now()));
                        }
                        Err(e) => {
                            state.notification = Some((
                                format!("Failed to run editor: {e}"),
                                Instant::now(),
                            ));
                        }
                    }
                    // Clean up temp file
                    let _ = std::fs::remove_file(&tmp_path);
                }
            } else {
                state.notification = Some((
                    "$EDITOR not set. Set EDITOR or VISUAL env var.".to_string(),
                    Instant::now(),
                ));
            }
        }

        // Data mutations: acquire lock, perform mutation, refresh
        Action::CreateStory {
            title,
            priority,
            labels,
            assignee,
        } => {
            let result = crate::lock::with_project_lock(root, || {
                let story = crate::storage::create_story(root, &title)?;
                let mut events = Vec::new();
                if let Some(p) = &priority {
                    events.push(crate::domain::StoryEvent::StoryPrioritySet {
                        at: crate::storage::now(),
                        priority: p.clone(),
                    });
                }
                if !labels.is_empty() {
                    events.push(crate::domain::StoryEvent::StoryLabelsSet {
                        at: crate::storage::now(),
                        labels: labels.clone(),
                    });
                }
                if let Some(a) = &assignee {
                    events.push(crate::domain::StoryEvent::StoryAssigned {
                        at: crate::storage::now(),
                        member_id: a.clone(),
                    });
                }
                if !events.is_empty() {
                    crate::storage::write_story_events(root, &story.id, &events)?;
                }
                Ok(story.id)
            });
            match result {
                Ok(id) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("Created {id}"), Instant::now()));
                    // Close create form modal
                    if let Some(Modal::CreateForm) = state.focus.top_modal() {
                        state.focus.pop_modal();
                        modal_components.create_form = None;
                    }
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Create failed: {e}"), Instant::now()));
                }
            }
        }

        Action::MoveStory { id, target_state } => {
            let result = crate::lock::with_project_lock(root, || {
                // Check if target state is CLOSED -- if so, we need to archive
                let states = crate::storage::load_state_map(root)?;
                let state_def = states.get(&target_state).ok_or_else(|| {
                    AppError::Validation(format!("state `{target_state}` is not defined"))
                })?;

                let now = crate::storage::now();
                let mut events = vec![crate::domain::StoryEvent::StoryStateChanged {
                    at: now.clone(),
                    state: target_state.clone(),
                }];

                if state_def.super_state == SuperState::Closed {
                    // Clear awaiting if set
                    let snapshot = crate::storage::load_open_story_snapshot(root, &id)?;
                    if snapshot.awaiting.is_some() {
                        events.push(crate::domain::StoryEvent::StoryAwaitingCleared {
                            at: now.clone(),
                        });
                    }
                    events.push(crate::domain::StoryEvent::StoryClosedAndArchived {
                        at: now,
                        state: target_state.clone(),
                    });
                }

                crate::storage::write_story_events(root, &id, &events)?;

                // Archive MUST happen inside the lock closure
                if state_def.super_state == SuperState::Closed {
                    crate::storage::archive_story(root, &id)?;
                }

                Ok(())
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("{id} moved to {target_state}"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Move failed: {e}"), Instant::now()));
                }
            }
        }

        Action::UpdateTitle { id, title } => {
            let result = crate::lock::with_project_lock(root, || {
                crate::storage::write_story_events(
                    root,
                    &id,
                    &[crate::domain::StoryEvent::StoryCommentAdded {
                        at: crate::storage::now(),
                        text: format!("Title updated to: {title}"),
                    }],
                )
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("{id} title updated"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Update failed: {e}"), Instant::now()));
                }
            }
        }

        Action::SetPriority { id, priority } => {
            let result = crate::lock::with_project_lock(root, || {
                crate::storage::write_story_events(
                    root,
                    &id,
                    &[crate::domain::StoryEvent::StoryPrioritySet {
                        at: crate::storage::now(),
                        priority: priority.clone(),
                    }],
                )
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("{id} priority set"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Priority update failed: {e}"), Instant::now()));
                }
            }
        }

        Action::SetLabels { id, labels } => {
            let result = crate::lock::with_project_lock(root, || {
                crate::storage::write_story_events(
                    root,
                    &id,
                    &[crate::domain::StoryEvent::StoryLabelsSet {
                        at: crate::storage::now(),
                        labels: labels.clone(),
                    }],
                )
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("{id} labels updated"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Labels update failed: {e}"), Instant::now()));
                }
            }
        }

        Action::AssignStory { id, assignee } => {
            let result = crate::lock::with_project_lock(root, || {
                crate::storage::write_story_events(
                    root,
                    &id,
                    &[crate::domain::StoryEvent::StoryAssigned {
                        at: crate::storage::now(),
                        member_id: assignee.clone(),
                    }],
                )
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("{id} assigned to {assignee}"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Assign failed: {e}"), Instant::now()));
                }
            }
        }

        Action::AddComment { id, text } => {
            let result = crate::lock::with_project_lock(root, || {
                crate::storage::write_story_events(
                    root,
                    &id,
                    &[crate::domain::StoryEvent::StoryCommentAdded {
                        at: crate::storage::now(),
                        text: text.clone(),
                    }],
                )
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("{id} comment added"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Comment failed: {e}"), Instant::now()));
                }
            }
        }

        Action::SetAwaiting { id, reason } => {
            let result = crate::lock::with_project_lock(root, || {
                crate::storage::write_story_events(
                    root,
                    &id,
                    &[crate::domain::StoryEvent::StoryAwaitingSet {
                        at: crate::storage::now(),
                        awaiting: reason.clone(),
                    }],
                )
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("{id} awaiting: {reason}"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Awaiting set failed: {e}"), Instant::now()));
                }
            }
        }

        Action::ClearAwaiting { id } => {
            let result = crate::lock::with_project_lock(root, || {
                crate::storage::write_story_events(
                    root,
                    &id,
                    &[crate::domain::StoryEvent::StoryAwaitingCleared {
                        at: crate::storage::now(),
                    }],
                )
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    state.notification =
                        Some((format!("{id} unblocked"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Clear awaiting failed: {e}"), Instant::now()));
                }
            }
        }
    }

    Ok(())
}

/// Render the current state.
#[allow(clippy::too_many_arguments)]
fn render(
    frame: &mut Frame,
    state: &AppState,
    _theme: &Theme,
    board: &mut Board,
    filter_bar: &mut FilterBar,
    dashboard: &mut Dashboard,
    status_bar: &mut StatusBar,
    modal_components: &mut ModalComponents,
) {
    let area = frame.area();

    // Minimum size check: show a message instead of the full UI
    if area.width < 40 || area.height < 10 {
        let msg = ratatui::text::Line::from(ratatui::text::Span::styled(
            "Terminal too small (min 40x10)",
            ratatui::style::Style::default()
                .fg(ratatui::style::Color::Yellow)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
        frame.render_widget(
            ratatui::widgets::Paragraph::new(msg)
                .alignment(ratatui::layout::Alignment::Center),
            area,
        );
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Fill(1),   // Main content
        Constraint::Length(1), // Status bar
    ])
    .split(area);

    let content_area = chunks[0];
    let status_area = chunks[1];

    // Main content
    match state.view {
        View::Board => {
            // Split content_area: 1 line for filter bar, rest for board
            let board_chunks = Layout::vertical([
                Constraint::Length(1), // Filter bar
                Constraint::Fill(1),   // Board
            ])
            .split(content_area);

            filter_bar.render(frame, board_chunks[0], state);
            board.render(frame, board_chunks[1], state);
        }
        View::Dashboard => {
            dashboard.render(frame, content_area, state);
        }
    }

    // Render modal overlays on top and track the top modal rect
    modal_components.modal_rect = None;
    // Clone modal list to avoid borrow conflict with modal_components
    let modals: Vec<Modal> = state.focus.modals.clone();
    for modal in &modals {
        match modal {
            Modal::StoryDetail { .. } => {
                if let Some(ref mut detail) = modal_components.story_detail {
                    detail.render(frame, area, state);
                    modal_components.modal_rect =
                        Some(super::components::modal::centered_modal_rect(area));
                }
            }
            Modal::CreateForm => {
                if let Some(ref mut form) = modal_components.create_form {
                    form.render(frame, area, state);
                    modal_components.modal_rect =
                        Some(super::components::modal::centered_modal_rect(area));
                }
            }
            Modal::Help => {
                if let Some(ref mut help) = modal_components.help {
                    help.render(frame, area, state);
                    modal_components.modal_rect =
                        Some(super::components::modal::centered_modal_rect(area));
                }
            }
        }
    }

    // Status bar
    status_bar.render(frame, status_area, state);
}

/// Allow DataStore to be moved out via `std::mem::take` for fallback on reload errors.
impl Default for DataStore {
    fn default() -> Self {
        Self {
            states: Vec::new(),
            state_map: std::collections::BTreeMap::new(),
            stories: Vec::new(),
            prefix: String::new(),
            members: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::action::View;
    use crate::tui::data::DataStore;
    use crate::tui::focus::{FocusStack, FocusTarget, Modal};
    use crate::tui::state::AppState;

    fn make_state() -> AppState {
        let data = DataStore::from_test_data(vec![], vec![], "SH".to_string(), vec![]);
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

    // =======================================================================
    // QA: Focus priority (BUG FIX verification)
    // Modals must always take priority over filter_bar_focused.
    // =======================================================================

    #[test]
    fn modal_takes_priority_over_filter_bar_focused() {
        let mut state = make_state();
        state.filter_bar_focused = true;
        state.focus.push_modal(Modal::Help);

        // With the bug fixed, Help modal should capture input
        let ctx = determine_key_context(&state);
        assert_eq!(ctx, KeyContext::Help,
            "Modal must take priority over filter_bar_focused");
    }

    #[test]
    fn filter_bar_focused_works_when_no_modal() {
        let mut state = make_state();
        state.filter_bar_focused = true;
        state.view = View::Board;

        let ctx = determine_key_context(&state);
        assert_eq!(ctx, KeyContext::FilterBarFocused);
    }

    #[test]
    fn global_context_when_nothing_focused() {
        let state = make_state();
        let ctx = determine_key_context(&state);
        assert_eq!(ctx, KeyContext::Global);
    }

    #[test]
    fn story_detail_modal_context() {
        let mut state = make_state();
        state.focus.push_modal(Modal::StoryDetail {
            story_id: "SH-1".to_string(),
        });
        let ctx = determine_key_context(&state);
        assert_eq!(ctx, KeyContext::StoryDetail);
    }

    #[test]
    fn create_form_modal_context() {
        let mut state = make_state();
        state.focus.push_modal(Modal::CreateForm);
        let ctx = determine_key_context(&state);
        assert_eq!(ctx, KeyContext::CreateForm);
    }

    #[test]
    fn nested_modals_top_wins() {
        let mut state = make_state();
        state.focus.push_modal(Modal::StoryDetail {
            story_id: "SH-1".to_string(),
        });
        state.focus.push_modal(Modal::Help);

        let ctx = determine_key_context(&state);
        assert_eq!(ctx, KeyContext::Help, "Top modal should determine context");
    }

    #[test]
    fn filter_bar_on_dashboard_view_is_global() {
        let mut state = make_state();
        state.filter_bar_focused = true;
        state.view = View::Dashboard; // filter bar only active on Board

        let ctx = determine_key_context(&state);
        assert_eq!(ctx, KeyContext::Global,
            "filter_bar_focused should be ignored on Dashboard view");
    }
}
