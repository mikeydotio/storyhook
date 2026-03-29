use std::path::Path;
use std::time::Instant;

use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::domain::SuperState;
use crate::error::AppError;

use super::action::{Action, View};
use super::components::board::Board;
use super::components::create_form::CreateForm;
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
    let status_bar = StatusBar::new();
    let mut modal_components = ModalComponents::default();

    let result = main_loop(
        &mut term,
        &mut state,
        &rx,
        root,
        &theme,
        &mut board,
        &status_bar,
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
}

#[allow(clippy::too_many_arguments)]
fn main_loop(
    term: &mut ratatui::DefaultTerminal,
    state: &mut AppState,
    rx: &std::sync::mpsc::Receiver<Event>,
    root: &Path,
    theme: &Theme,
    board: &mut Board,
    status_bar: &StatusBar,
    modal_components: &mut ModalComponents,
) -> Result<(), AppError> {
    // Initial render
    term.draw(|frame| render(frame, state, theme, board, status_bar, modal_components))?;

    while state.running {
        let event = match rx.recv() {
            Ok(e) => e,
            Err(_) => break, // All senders dropped
        };

        // Update terminal size on resize events
        if let Event::Resize(w, h) = &event {
            state.terminal_size = (*w, *h);
        }

        let actions = route_event(event, state, board, modal_components);

        for action in actions {
            dispatch(action, state, root, board, modal_components)?;
        }

        // Expire notifications (3s timeout)
        if let Some((_, created_at)) = &state.notification
            && created_at.elapsed().as_secs() >= 3
        {
            state.notification = None;
        }

        term.draw(|frame| render(frame, state, theme, board, status_bar, modal_components))?;
    }

    Ok(())
}

/// Route an event to zero or more actions based on current focus context.
fn route_event(
    event: Event,
    state: &AppState,
    board: &mut Board,
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
                        KeyContext::Global => {
                            match state.view {
                                View::Board => {
                                    // First check keymap-level board bindings (n, /)
                                    if let Some(action) = keymap::map_key(key, KeyContext::Board) {
                                        return vec![action];
                                    }
                                    // Then delegate to board component for navigation keys
                                    return board.handle_key(key, state);
                                }
                                View::Dashboard => {
                                    // Dashboard has no extra bindings yet
                                }
                            }
                            vec![]
                        }
                        _ => vec![],
                    }
                }
            }
        }
        Event::Mouse(_mouse) => {
            // Mouse handling will be implemented in later waves
            vec![]
        }
        Event::Resize(w, h) => {
            // Resize is handled inline, no action needed
            // State will be updated during dispatch
            vec![Action::Notify(format!("Resized to {w}x{h}"))]
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
    // If filter bar is focused, it gets priority
    if state.filter_bar_focused {
        return KeyContext::FilterBarFocused;
    }

    // If a modal is open, it captures all input
    if let Some(modal) = state.focus.top_modal() {
        return match modal {
            Modal::StoryDetail { .. } => KeyContext::StoryDetail,
            Modal::CreateForm => KeyContext::CreateForm,
            Modal::Help => KeyContext::Help,
        };
    }

    // No modal: global context (view-specific bindings checked in route_event)
    KeyContext::Global
}

/// Dispatch a single action, mutating AppState.
fn dispatch(
    action: Action,
    state: &mut AppState,
    root: &Path,
    board: &mut Board,
    modal_components: &mut ModalComponents,
) -> Result<(), AppError> {
    match action {
        Action::Quit => {
            state.running = false;
        }

        Action::SwitchView(view) => {
            state.view = view.clone();
            state.focus.base = match view {
                View::Dashboard => FocusTarget::Dashboard,
                View::Board => FocusTarget::Board,
            };
        }

        Action::ToggleHelp => {
            if let Some(Modal::Help) = state.focus.top_modal() {
                state.focus.pop_modal();
            } else {
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
                    Modal::Help => {}
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
        }

        Action::ClearFilter(index) => {
            if index < state.filters.len() {
                state.filters.remove(index);
            }
        }

        Action::ClearAllFilters => {
            state.filters.clear();
            state.filter_bar_focused = false;
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
fn render(
    frame: &mut Frame,
    state: &AppState,
    theme: &Theme,
    board: &Board,
    status_bar: &StatusBar,
    modal_components: &ModalComponents,
) {
    let area = frame.area();

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
            board.render(frame, content_area, state);
        }
        View::Dashboard => {
            // Dashboard placeholder until Wave 4
            let view_label = "Dashboard";
            let story_count = state.data.story_count();

            let content = Line::from(vec![
                Span::styled(view_label, theme.section_header),
                Span::raw("  "),
                Span::styled(
                    format!("{story_count} stories"),
                    theme.section_count,
                ),
            ]);

            let vertical = Layout::vertical([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .split(content_area);

            frame.render_widget(
                Paragraph::new(content).alignment(Alignment::Center),
                vertical[1],
            );
        }
    }

    // Render modal overlays on top
    for modal in &state.focus.modals {
        match modal {
            Modal::StoryDetail { .. } => {
                if let Some(ref detail) = modal_components.story_detail {
                    detail.render(frame, area, state);
                }
            }
            Modal::CreateForm => {
                if let Some(ref form) = modal_components.create_form {
                    form.render(frame, area, state);
                }
            }
            Modal::Help => {
                // Help overlay will be implemented in a later wave
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
