use std::path::Path;
use std::time::Instant;

use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::error::AppError;

use super::action::{Action, View};
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

    let result = main_loop(&mut term, &mut state, &rx, root, &theme);

    event_source.stop();
    terminal::restore();
    result
}

fn main_loop(
    term: &mut ratatui::DefaultTerminal,
    state: &mut AppState,
    rx: &std::sync::mpsc::Receiver<Event>,
    root: &Path,
    theme: &Theme,
) -> Result<(), AppError> {
    // Initial render
    term.draw(|frame| render(frame, state, theme))?;

    while state.running {
        let event = match rx.recv() {
            Ok(e) => e,
            Err(_) => break, // All senders dropped
        };

        let actions = route_event(event, state);

        for action in actions {
            dispatch(action, state, root)?;
        }

        // Expire notifications (3s timeout)
        if let Some((_, created_at)) = &state.notification
            && created_at.elapsed().as_secs() >= 3
        {
            state.notification = None;
        }

        term.draw(|frame| render(frame, state, theme))?;
    }

    Ok(())
}

/// Route an event to zero or more actions based on current focus context.
fn route_event(event: Event, state: &AppState) -> Vec<Action> {
    match event {
        Event::Key(key) => {
            let context = determine_key_context(state);
            match keymap::map_key(key, context) {
                Some(action) => vec![action],
                None => {
                    // For global context, also try board/dashboard specific bindings
                    if context == KeyContext::Global {
                        let view_context = match state.view {
                            View::Dashboard => KeyContext::Global, // Dashboard has no extra bindings yet
                            View::Board => KeyContext::Board,
                        };
                        if view_context != KeyContext::Global
                            && let Some(action) = keymap::map_key(key, view_context)
                        {
                            return vec![action];
                        }
                    }
                    vec![]
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
fn dispatch(action: Action, state: &mut AppState, root: &Path) -> Result<(), AppError> {
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
            state
                .focus
                .push_modal(Modal::StoryDetail { story_id: id });
        }

        Action::OpenCreateForm => {
            state.focus.push_modal(Modal::CreateForm);
        }

        Action::CloseModal => {
            state.focus.pop_modal();
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
                    // Stale modal protection: if a detail modal is open, check
                    // that the story still exists
                    if let Some(Modal::StoryDetail { story_id }) = state.focus.top_modal()
                        && state.data.find_story(story_id).is_none()
                    {
                        let id = story_id.clone();
                        state.focus.pop_modal();
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
            // Handled by Board component in Wave 2
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
                    state.notification =
                        Some((format!("Created {id}"), Instant::now()));
                    state.focus.pop_modal(); // Close create form
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Create failed: {e}"), Instant::now()));
                }
            }
        }

        Action::MoveStory { id, target_state } => {
            let result = crate::lock::with_project_lock(root, || {
                crate::storage::write_story_events(
                    root,
                    &id,
                    &[crate::domain::StoryEvent::StoryStateChanged {
                        at: crate::storage::now(),
                        state: target_state.clone(),
                    }],
                )
            });
            match result {
                Ok(()) => {
                    state.data = DataStore::load(root).unwrap_or(std::mem::take(&mut state.data));
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
            // Title update requires rewriting events (storyhook doesn't have a title-change event,
            // so we add a comment noting the title change and rewrite)
            // For now, we'll add a comment and handle title editing in the StoryDetail component.
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

/// Render the current state. For Wave 1, this shows view name + story count.
/// Full component rendering comes in Wave 2.
fn render(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let area = frame.area();

    let chunks = Layout::vertical([
        Constraint::Fill(1),   // Main content
        Constraint::Length(1), // Status bar
    ])
    .split(area);

    // Main content: view label + story count
    let view_label = match state.view {
        View::Dashboard => "Dashboard",
        View::Board => "Board",
    };
    let story_count = state.data.story_count();

    let content = Line::from(vec![
        Span::styled(view_label, theme.section_header),
        Span::raw("  "),
        Span::styled(
            format!("{story_count} stories"),
            theme.section_count,
        ),
    ]);

    let content_area = chunks[0];
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

    // Status bar
    let mut status_spans = vec![
        Span::styled(" q", theme.status_bar_keys),
        Span::styled(" quit  ", theme.status_bar),
        Span::styled("?", theme.status_bar_keys),
        Span::styled(" help  ", theme.status_bar),
        Span::styled("1", theme.status_bar_keys),
        Span::styled(" dash  ", theme.status_bar),
        Span::styled("2", theme.status_bar_keys),
        Span::styled(" board", theme.status_bar),
    ];

    // Show notification if any
    if let Some((ref msg, _)) = state.notification {
        status_spans.push(Span::raw("  "));
        status_spans.push(Span::styled(
            msg.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(status_spans)),
        chunks[1],
    );
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
