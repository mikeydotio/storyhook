use std::path::Path;
use std::time::Instant;

use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::cli::{HistoryAction, Invocation, StateAction};
use crate::domain::{FieldEdit, StoryEvent, StorySnapshot, SuperState};
use crate::error::AppError;
use crate::invoke::{InvokeRequest, Invoker, StoreInvoker};
use crate::output::Response;

use super::action::{Action, UndoEntry, View};
use super::components::Component;
use super::components::board::Board;
use super::components::create_form::CreateForm;
use super::components::dashboard::Dashboard;
use super::components::filter_bar::FilterBar;
use super::components::graph::GraphComponent;
use super::components::help::Help;
use super::components::states_editor::StatesEditor;
use super::components::status_bar::StatusBar;
use super::components::story_detail::StoryDetail;
use super::data::DataStore;
use super::event::{Event, EventSource};
use super::focus::{FocusTarget, Modal};
use super::keymap::{self, KeyContext};
use super::state::AppState;
use super::terminal;
use super::theme::Theme;

/// Run the TUI application.
///
/// `root` is the directory the project is resolved from, and nothing else reads
/// it: every read and every mutation goes through [`Invoker`]. Live updates come
/// from the store's own change token rather than from a watcher over `root` —
/// see [`EventSource`].
pub fn run(root: &Path) -> Result<(), AppError> {
    // `None` for the store flag: `main` publishes any `--store-path` into
    // `$STORYHOOK_STORE_PATH` before the TUI is dispatched, so this resolves
    // the store the caller named.
    let environment = crate::env::Environment::from_process(None)?;
    let event_environment = environment.clone();
    let store = crate::invoke::open_store(&environment)?;
    let invoker = StoreInvoker::new(&store, root, environment);
    let data = DataStore::load(&invoker)?;
    let mut state = AppState::new(data);
    let theme = Theme::from_env();

    let mut term = terminal::init()?;
    let (event_source, rx) = EventSource::new(&event_environment);

    // Get initial terminal size
    if let Ok(size) = term.size() {
        state.terminal_size = (size.width, size.height);
    }

    // Create components
    let mut board = Board::new();
    let mut filter_bar = FilterBar::new();
    let mut dashboard = Dashboard::new();
    let mut graph = GraphComponent::new();
    let mut status_bar = StatusBar::new();
    let mut modal_components = ModalComponents::default();

    let result = main_loop(
        &mut term,
        &mut state,
        &rx,
        &invoker,
        &theme,
        &mut board,
        &mut filter_bar,
        &mut dashboard,
        &mut graph,
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
    states_editor: Option<StatesEditor>,
    help: Option<Help>,
    /// The rect of the top modal from the last render, for click-outside detection.
    modal_rect: Option<Rect>,
}

#[allow(clippy::too_many_arguments)]
fn main_loop(
    term: &mut ratatui::DefaultTerminal,
    state: &mut AppState,
    rx: &std::sync::mpsc::Receiver<Event>,
    invoker: &dyn Invoker,
    theme: &Theme,
    board: &mut Board,
    filter_bar: &mut FilterBar,
    dashboard: &mut Dashboard,
    graph: &mut GraphComponent,
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
            graph,
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

        let actions = route_event(
            event,
            state,
            board,
            filter_bar,
            dashboard,
            graph,
            modal_components,
        );

        for action in actions {
            dispatch(action, state, invoker, term, board, graph, modal_components)?;
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
                graph,
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
    graph: &mut GraphComponent,
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
                        KeyContext::StatesEditor => {
                            if let Some(ref mut editor) = modal_components.states_editor {
                                return editor.handle_key(key, state);
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
                            View::Graph => {
                                // First check keymap-level graph bindings (n)
                                if let Some(action) = keymap::map_key(key, KeyContext::Graph) {
                                    return vec![action];
                                }
                                // Then delegate to graph component for navigation keys
                                graph.handle_key(key, state)
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
                View::Dashboard => dashboard.handle_mouse(mouse, state),
                View::Graph => graph.handle_mouse(mouse, state),
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
            Modal::StatesEditor => KeyContext::StatesEditor,
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

/// Runs one invocation through the seam, with the project's event hooks
/// suppressed.
///
/// The TUI has never fired them, and routing through the seam must not start:
/// a hook is an arbitrary shell command, and a board that ran one on every
/// keystroke would be a surprise nobody asked for. Every request the TUI makes
/// carries `no_hooks`, so the behaviour is a property of this function rather
/// than of each call site remembering.
fn invoke(invoker: &dyn Invoker, invocation: Invocation) -> Result<Response, AppError> {
    invoker.invoke(InvokeRequest::new(invocation).no_hooks(true))
}

/// The story a mutation answered with.
fn story_of(response: Response) -> Result<StorySnapshot, AppError> {
    match response {
        Response::Story(view) => Ok(view.story),
        other => Err(AppError::Storage(format!(
            "internal: expected a story, got {other:?}"
        ))),
    }
}

/// The text a `Response::Message` carries.
fn message_of(response: Response) -> Result<String, AppError> {
    match response {
        Response::Message(message) => Ok(message),
        other => Err(AppError::Storage(format!(
            "internal: expected a message, got {other:?}"
        ))),
    }
}

/// A `story set` invocation touching exactly one field.
///
/// `Invocation::SetFields` has eleven, ten of which are always absent here —
/// the TUI edits one thing at a time — so the ten `None`s are written once.
fn set_field(id: &str, title: Option<String>, description: Option<String>) -> Invocation {
    Invocation::SetFields {
        id: id.to_string(),
        title,
        state: None,
        priority: None,
        assignee: None,
        labels: None,
        blocked: None,
        unblocked: false,
        json: None,
        story_type: None,
        description,
    }
}

/// Load the current events for a story, returning an empty vec if the story doesn't exist.
fn snapshot_for_undo(invoker: &dyn Invoker, story_id: &str) -> Vec<StoryEvent> {
    match invoke(
        invoker,
        Invocation::History {
            action: HistoryAction::Read {
                id: story_id.to_string(),
            },
        },
    ) {
        Ok(Response::StoryHistory(events)) => events,
        _ => Vec::new(),
    }
}

/// Push an undo entry and clear the redo stack (any new mutation invalidates redo history).
fn push_undo(
    state: &mut AppState,
    description: String,
    story_id: String,
    events_before: Vec<StoryEvent>,
) {
    state.undo_stack.push(UndoEntry {
        description,
        story_id,
        events_before,
    });
    state.redo_stack.clear();
}

/// Create a story with the given enrichment fields, through the seam.
///
/// One `story new` invocation, which is where the enrichment rules already
/// live: an unknown assignee aborts the whole creation before anything is
/// written, and the resolved member's canonical id — not the raw,
/// possibly-a-GitHub-handle input — is what gets stored.
///
/// Extracted from the `Action::CreateStory` dispatch arm so it can be
/// exercised by tests without a real terminal (`dispatch` takes one and
/// can't be unit-tested directly).
fn create_story_mutation(
    invoker: &dyn Invoker,
    title: &str,
    priority: Option<crate::domain::Priority>,
    labels: &[String],
    assignee: Option<&str>,
    description: Option<&str>,
) -> Result<String, AppError> {
    let response = invoke(
        invoker,
        Invocation::New {
            title: title.to_string(),
            state: None,
            story_type: None,
            description: description.map(str::to_string),
            priority: priority.map(|p| p.as_str().to_string()),
            labels: (!labels.is_empty()).then(|| labels.to_vec()),
            assignee: assignee.map(str::to_string),
        },
    )?;
    Ok(story_of(response)?.id)
}

/// Assign an existing story to a member, through the seam.
///
/// `assignee` may be a member id or a GitHub handle; an unknown one aborts the
/// mutation with nothing written, and a known one is normalized to its
/// canonical member id.
///
/// Extracted from the `Action::AssignStory` dispatch arm for the same
/// testability reason as [`create_story_mutation`].
fn assign_story_mutation(invoker: &dyn Invoker, id: &str, assignee: &str) -> Result<(), AppError> {
    invoke(
        invoker,
        Invocation::Assign {
            id: id.to_string(),
            member: assignee.to_string(),
        },
    )
    .map(|_| ())
}

/// Dispatch a single action, mutating AppState.
#[allow(clippy::too_many_arguments)]
/// Shared tail of every statuses-editor mutation: report what happened, then
/// reload so the board's columns, the editor's rows, and any story the edit
/// moved all reflect the new configuration.
fn finish_state_edit(
    state: &mut AppState,
    board: &mut Board,
    graph: &mut GraphComponent,
    modal_components: &mut ModalComponents,
    invoker: &dyn Invoker,
    result: Result<String, AppError>,
    action_label: &str,
) {
    match result {
        Ok(message) => {
            state.notification = Some((message, Instant::now()));
            state.data = DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
            board.on_state_change(state);
            graph.on_state_change(state);
            if let Some(ref mut editor) = modal_components.states_editor {
                editor.on_state_change(state);
            }
        }
        Err(error) => {
            state.notification = Some((format!("{action_label} failed: {error}"), Instant::now()));
        }
    }
}

fn dispatch(
    action: Action,
    state: &mut AppState,
    invoker: &dyn Invoker,
    term: &mut ratatui::DefaultTerminal,
    board: &mut Board,
    graph: &mut GraphComponent,
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
                View::Graph => FocusTarget::Graph,
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
            state.focus.push_modal(Modal::StoryDetail { story_id: id });
        }

        Action::OpenCreateForm => {
            modal_components.create_form = Some(CreateForm::new());
            state.focus.push_modal(Modal::CreateForm);
        }

        Action::OpenStatesEditor => {
            if !matches!(state.focus.top_modal(), Some(Modal::StatesEditor)) {
                modal_components.states_editor = Some(StatesEditor::new());
                state.focus.push_modal(Modal::StatesEditor);
            }
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
                    Modal::StatesEditor => {
                        modal_components.states_editor = None;
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
            match DataStore::load(invoker) {
                Ok(data) => {
                    state.data = data;
                    // Notify board of state change so it can reclamp cursor
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    // Stale modal protection: if a detail modal is open, check
                    // that the story still exists
                    if let Some(Modal::StoryDetail { story_id }) = state.focus.top_modal()
                        && state.data.find_story(story_id).is_none()
                    {
                        let id = story_id.clone();
                        state.focus.pop_modal();
                        modal_components.story_detail = None;
                        state.notification =
                            Some((format!("Story {id} no longer open"), Instant::now()));
                    }
                }
                Err(e) => {
                    state.notification = Some((format!("Refresh failed: {e}"), Instant::now()));
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
            graph.on_state_change(state);
        }

        Action::ClearFilter(index) => {
            if index < state.filters.len() {
                state.filters.remove(index);
            }
            board.on_state_change(state);
            graph.on_state_change(state);
        }

        Action::ClearAllFilters => {
            state.filters.clear();
            state.filter_bar_focused = false;
            board.on_state_change(state);
            graph.on_state_change(state);
        }

        Action::OpenEditor { id } => {
            // $EDITOR integration: suspend TUI, run editor, resume TUI
            let editor = std::env::var("EDITOR")
                .or_else(|_| std::env::var("VISUAL"))
                .ok();

            if let Some(editor_cmd) = editor {
                // Create temp file with placeholder text
                let tmp_dir = std::env::temp_dir();
                let tmp_path = tmp_dir.join(format!("storyhook-comment-{id}.txt"));
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
                                            invoker,
                                            term,
                                            board,
                                            graph,
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
                            state.notification =
                                Some((format!("Failed to run editor: {e}"), Instant::now()));
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
            description,
        } => {
            let desc = format!("Created story: {title}");
            let result = create_story_mutation(
                invoker,
                &title,
                priority,
                &labels,
                assignee.as_deref(),
                description.as_deref(),
            );
            match result {
                Ok(id) => {
                    // Story didn't exist before creation: events_before is empty
                    push_undo(state, desc, id.clone(), Vec::new());
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification = Some((format!("Created {id}"), Instant::now()));
                    // Close create form modal
                    if let Some(Modal::CreateForm) = state.focus.top_modal() {
                        state.focus.pop_modal();
                        modal_components.create_form = None;
                    }
                }
                Err(e) => {
                    state.notification = Some((format!("Create failed: {e}"), Instant::now()));
                }
            }
        }

        Action::MoveStory { id, target_state } => {
            // Snapshot before mutation for potential undo
            let events_before = snapshot_for_undo(invoker, &id);
            // The story the move answers with says whether it closed: a move
            // into a CLOSED state archives, and an archived story cannot be
            // restored by replaying its log.
            let result = invoke(
                invoker,
                Invocation::SetState {
                    id: id.clone(),
                    state: target_state.clone(),
                    comment: None,
                    if_state: None,
                },
            )
            .and_then(story_of)
            .map(|story| story.superstate == SuperState::Closed);
            match result {
                Ok(is_close) => {
                    if is_close {
                        state.notification = Some((
                            format!("{id} closed (close cannot be undone)"),
                            Instant::now(),
                        ));
                    } else {
                        push_undo(
                            state,
                            format!("Moved {id} to {target_state}"),
                            id.clone(),
                            events_before,
                        );
                        state.notification =
                            Some((format!("{id} moved to {target_state}"), Instant::now()));
                    }
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                }
                Err(e) => {
                    state.notification = Some((format!("Move failed: {e}"), Instant::now()));
                }
            }
        }

        Action::UpdateTitle { id, title } => {
            let events_before = snapshot_for_undo(invoker, &id);
            let result = invoke(invoker, set_field(&id, Some(title.clone()), None)).map(|_| ());
            match result {
                Ok(()) => {
                    push_undo(
                        state,
                        format!("{id} title updated"),
                        id.clone(),
                        events_before,
                    );
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification = Some((format!("{id} title updated"), Instant::now()));
                }
                Err(e) => {
                    state.notification = Some((format!("Update failed: {e}"), Instant::now()));
                }
            }
        }

        Action::SetDescription { id, description } => {
            let events_before = snapshot_for_undo(invoker, &id);
            let result =
                invoke(invoker, set_field(&id, None, Some(description.clone()))).map(|_| ());
            match result {
                Ok(()) => {
                    push_undo(
                        state,
                        format!("{id} description updated"),
                        id.clone(),
                        events_before,
                    );
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification =
                        Some((format!("{id} description updated"), Instant::now()));
                }
                Err(e) => {
                    state.notification = Some((format!("Update failed: {e}"), Instant::now()));
                }
            }
        }

        Action::SetPriority { id, priority } => {
            let events_before = snapshot_for_undo(invoker, &id);
            let result = invoke(
                invoker,
                Invocation::SetPriority {
                    id: id.clone(),
                    priority: priority.as_str().to_string(),
                },
            )
            .map(|_| ());
            match result {
                Ok(()) => {
                    push_undo(
                        state,
                        format!("{id} priority set to {}", priority.as_str()),
                        id.clone(),
                        events_before,
                    );
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification = Some((format!("{id} priority set"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Priority update failed: {e}"), Instant::now()));
                }
            }
        }

        Action::SetLabels { id, labels } => {
            let events_before = snapshot_for_undo(invoker, &id);
            // The editor hands over the label set it wants, and the seam speaks
            // in additions and removals — so the difference against what the
            // story carries now is computed here rather than a whole-set write
            // being invented at the seam for one caller.
            let current: Vec<String> = state
                .data
                .find_story(&id)
                .map(|story| story.labels.clone())
                .unwrap_or_default();
            let remove: Vec<String> = current
                .into_iter()
                .filter(|label| !labels.contains(label))
                .collect();
            let result = invoke(
                invoker,
                Invocation::SetLabels {
                    id: id.clone(),
                    add: labels.clone(),
                    remove,
                },
            )
            .map(|_| ());
            match result {
                Ok(()) => {
                    push_undo(
                        state,
                        format!("{id} labels updated"),
                        id.clone(),
                        events_before,
                    );
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification = Some((format!("{id} labels updated"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Labels update failed: {e}"), Instant::now()));
                }
            }
        }

        Action::AssignStory { id, assignee } => {
            let events_before = snapshot_for_undo(invoker, &id);
            let result = assign_story_mutation(invoker, &id, &assignee);
            match result {
                Ok(()) => {
                    push_undo(
                        state,
                        format!("{id} assigned to {assignee}"),
                        id.clone(),
                        events_before,
                    );
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification =
                        Some((format!("{id} assigned to {assignee}"), Instant::now()));
                }
                Err(e) => {
                    state.notification = Some((format!("Assign failed: {e}"), Instant::now()));
                }
            }
        }

        Action::AddComment { id, text } => {
            let events_before = snapshot_for_undo(invoker, &id);
            let result = invoke(
                invoker,
                Invocation::Comment {
                    id: id.clone(),
                    text: text.clone(),
                },
            )
            .map(|_| ());
            match result {
                Ok(()) => {
                    push_undo(
                        state,
                        format!("{id} comment added"),
                        id.clone(),
                        events_before,
                    );
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification = Some((format!("{id} comment added"), Instant::now()));
                }
                Err(e) => {
                    state.notification = Some((format!("Comment failed: {e}"), Instant::now()));
                }
            }
        }

        Action::SetAwaiting { id, reason } => {
            let events_before = snapshot_for_undo(invoker, &id);
            let result = invoke(
                invoker,
                Invocation::SetAwaiting {
                    id: id.clone(),
                    awaiting: reason.clone(),
                },
            )
            .map(|_| ());
            match result {
                Ok(()) => {
                    push_undo(
                        state,
                        format!("{id} awaiting: {reason}"),
                        id.clone(),
                        events_before,
                    );
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification = Some((format!("{id} awaiting: {reason}"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Awaiting set failed: {e}"), Instant::now()));
                }
            }
        }

        Action::ClearAwaiting { id } => {
            let events_before = snapshot_for_undo(invoker, &id);
            let result = invoke(invoker, Invocation::ClearAwaiting { id: id.clone() }).map(|_| ());
            match result {
                Ok(()) => {
                    push_undo(state, format!("{id} unblocked"), id.clone(), events_before);
                    state.data =
                        DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                    board.on_state_change(state);
                    graph.on_state_change(state);
                    state.notification = Some((format!("{id} unblocked"), Instant::now()));
                }
                Err(e) => {
                    state.notification =
                        Some((format!("Clear awaiting failed: {e}"), Instant::now()));
                }
            }
        }

        // --- Project configuration (the statuses editor) ---
        //
        // These edit the project's state catalog through the same invocations
        // the CLI and the web dashboard use, so all three enforce one set of
        // rules. None of them push onto the undo stack: it replays a single
        // story's event log, and a status edit changes the project's
        // configuration — sometimes migrating stories as it goes.
        Action::AddState { slug, super_state } => {
            let result = invoke(
                invoker,
                Invocation::State {
                    action: StateAction::Add {
                        slug: slug.clone(),
                        superstate: super_state.as_str().to_string(),
                        role: None,
                        description: None,
                    },
                },
            );
            finish_state_edit(
                state,
                board,
                graph,
                modal_components,
                invoker,
                result.map(|_| format!("added status {slug}")),
                "Add status",
            );
        }

        Action::SetStateFields {
            slug,
            changes,
            move_stories_to,
        } => {
            let result = invoke(
                invoker,
                Invocation::State {
                    action: StateAction::Set {
                        slug: slug.clone(),
                        superstate: changes.super_state.map(|s| s.as_str().to_string()),
                        // `--role none` is how the grammar spells "clear".
                        role: match &changes.role {
                            FieldEdit::Keep => None,
                            FieldEdit::Clear => Some("none".to_string()),
                            FieldEdit::Set(value) => Some(value.clone()),
                        },
                        description: match &changes.description {
                            FieldEdit::Set(value) => Some(value.clone()),
                            _ => None,
                        },
                        clear_description: matches!(changes.description, FieldEdit::Clear),
                        move_stories_to: move_stories_to.clone(),
                    },
                },
            )
            .and_then(message_of);
            finish_state_edit(
                state,
                board,
                graph,
                modal_components,
                invoker,
                result,
                "Update status",
            );
        }

        Action::RemoveState {
            slug,
            move_stories_to,
        } => {
            let result = invoke(
                invoker,
                Invocation::State {
                    action: StateAction::Remove {
                        slug: slug.clone(),
                        move_stories_to: move_stories_to.clone(),
                    },
                },
            )
            .and_then(message_of);
            finish_state_edit(
                state,
                board,
                graph,
                modal_components,
                invoker,
                result,
                "Remove status",
            );
        }

        Action::ReorderStates { order } => {
            let result = invoke(
                invoker,
                Invocation::State {
                    action: StateAction::Reorder {
                        order: order.clone(),
                    },
                },
            );
            finish_state_edit(
                state,
                board,
                graph,
                modal_components,
                invoker,
                result.map(|_| "reordered statuses".to_string()),
                "Reorder statuses",
            );
        }

        Action::Undo => {
            if let Some(entry) = state.undo_stack.pop() {
                // Snapshot current state for redo
                let current_events = snapshot_for_undo(invoker, &entry.story_id);

                let story_id = entry.story_id.clone();
                let events_before = entry.events_before.clone();
                // An empty history means the story did not exist before the
                // mutation, and `Restore` reads that as "it should not exist
                // now" — so undoing a creation and undoing an edit are one
                // invocation rather than two code paths. Since the flip that
                // means the story is *deleted* rather than erased: the id stays
                // spent and `story show` still answers.
                let result = invoke(
                    invoker,
                    Invocation::History {
                        action: HistoryAction::Restore {
                            id: story_id,
                            events: events_before,
                        },
                    },
                )
                .map(|_| ());

                match result {
                    Ok(()) => {
                        state.redo_stack.push(UndoEntry {
                            description: entry.description.clone(),
                            story_id: entry.story_id,
                            events_before: current_events,
                        });
                        state.data =
                            DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                        board.on_state_change(state);
                        graph.on_state_change(state);
                        state.notification =
                            Some((format!("Undone: {}", entry.description), Instant::now()));
                    }
                    Err(e) => {
                        // Put entry back on undo stack since undo failed
                        state.undo_stack.push(entry);
                        state.notification = Some((format!("Undo failed: {e}"), Instant::now()));
                    }
                }
            } else {
                state.notification = Some(("Nothing to undo".to_string(), Instant::now()));
            }
        }

        Action::Redo => {
            if let Some(entry) = state.redo_stack.pop() {
                // Snapshot current state for undo
                let current_events = snapshot_for_undo(invoker, &entry.story_id);

                let story_id = entry.story_id.clone();
                let events_before = entry.events_before.clone();
                let result = invoke(
                    invoker,
                    Invocation::History {
                        action: HistoryAction::Restore {
                            id: story_id,
                            events: events_before,
                        },
                    },
                )
                .map(|_| ());

                match result {
                    Ok(()) => {
                        state.undo_stack.push(UndoEntry {
                            description: entry.description.clone(),
                            story_id: entry.story_id,
                            events_before: current_events,
                        });
                        state.data =
                            DataStore::load(invoker).unwrap_or(std::mem::take(&mut state.data));
                        board.on_state_change(state);
                        graph.on_state_change(state);
                        state.notification =
                            Some((format!("Redone: {}", entry.description), Instant::now()));
                    }
                    Err(e) => {
                        // Put entry back on redo stack since redo failed
                        state.redo_stack.push(entry);
                        state.notification = Some((format!("Redo failed: {e}"), Instant::now()));
                    }
                }
            } else {
                state.notification = Some(("Nothing to redo".to_string(), Instant::now()));
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
    graph: &mut GraphComponent,
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
            ratatui::widgets::Paragraph::new(msg).alignment(ratatui::layout::Alignment::Center),
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
        View::Graph => {
            graph.render(frame, content_area, state);
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
            Modal::StatesEditor => {
                if let Some(ref mut editor) = modal_components.states_editor {
                    editor.render(frame, area, state);
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

// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#[allow(clippy::disallowed_methods)]
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
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    // =======================================================================
    // Regression: #39 — create/assign mutations must validate assignee
    // against real members.
    //
    // Reconstructed onto the Invoker seam: the fixture is built with
    // invocations instead of `storage::` calls, and the mutations are handed
    // an `Invoker` instead of a project root. Every assertion is unchanged.
    // =======================================================================

    /// A project with one member whose canonical id differs from the GitHub
    /// handle it was created from — `Mikey-Ward` slugifies to `mikey-ward` —
    /// so "resolves the handle to the id" stays a real claim.
    ///
    /// Built on the store, because that is what the TUI runs against. The
    /// store's directory is a fixture of its own rather than
    /// `paths::store_path()`: these are in-process tests, and an in-process
    /// test cannot redirect `STORYHOOK_DATA_DIR` for itself — a helper that
    /// read the environment would open the developer's real database.
    struct TuiFixture {
        store: crate::store::SqliteStore,
        root: std::path::PathBuf,
        env: crate::env::Environment,
        _data: tempfile::TempDir,
        _repo: tempfile::TempDir,
    }

    impl TuiFixture {
        fn new() -> Self {
            use crate::store::Store as _;
            let data = tempfile::tempdir().unwrap();
            let repo = tempfile::tempdir().unwrap();
            let env = crate::env::Environment::at(data.path());
            let store = crate::store::SqliteStore::open(env.store_path()).unwrap();
            store.migrate().unwrap();
            let fixture = TuiFixture {
                store,
                root: repo.path().to_path_buf(),
                env,
                _data: data,
                _repo: repo,
            };
            fixture
                .invoker()
                .invoke(InvokeRequest::new(Invocation::Project {
                    action: crate::cli::ProjectAction::New(crate::cli::NewProjectRequest::Stated(
                        crate::cli::NewProjectSpec {
                            attach: crate::cli::Attach::Cwd,
                            prefix: "SH".to_string(),
                            name: None,
                            no_agents_md: true,
                        },
                    )),
                }))
                .unwrap();
            fixture
        }

        fn invoker(&self) -> StoreInvoker<'_, crate::store::SqliteStore> {
            StoreInvoker::new(&self.store, &self.root, self.env.clone())
        }
    }

    fn add_test_member(invoker: &dyn Invoker, handle: &str) {
        invoke(
            invoker,
            Invocation::MemberAdd {
                input: crate::cli::MemberInput::Github(handle.to_string()),
            },
        )
        .unwrap();
    }

    fn seed_story(invoker: &dyn Invoker, title: &str) -> String {
        create_story_mutation(invoker, title, None, &[], None, None).unwrap()
    }

    #[test]
    fn create_story_mutation_rejects_unknown_assignee_and_creates_no_story() {
        let fixture = TuiFixture::new();
        let invoker = fixture.invoker();

        let result =
            create_story_mutation(&invoker, "Bad assignee", None, &[], Some("nobody"), None);

        assert!(result.is_err(), "unknown assignee should be rejected");
        let store = DataStore::load(&invoker).unwrap();
        assert_eq!(
            store.story_count(),
            0,
            "no story should be written when the assignee is invalid"
        );
    }

    #[test]
    fn create_story_mutation_resolves_github_handle_to_member_id() {
        let fixture = TuiFixture::new();
        let invoker = fixture.invoker();
        add_test_member(&invoker, "Mikey-Ward");

        let id = create_story_mutation(
            &invoker,
            "Assigned story",
            None,
            &[],
            Some("Mikey-Ward"),
            None,
        )
        .expect("valid github handle should be accepted");

        let store = DataStore::load(&invoker).unwrap();
        let story = store.find_story(&id).expect("story should exist");
        assert_eq!(
            story.assignee.as_deref(),
            Some("mikey-ward"),
            "github handle should normalize to the canonical member id"
        );
    }

    #[test]
    fn assign_story_mutation_rejects_unknown_member_and_leaves_story_unassigned() {
        let fixture = TuiFixture::new();
        let invoker = fixture.invoker();
        let id = seed_story(&invoker, "Unassigned story");

        let result = assign_story_mutation(&invoker, &id, "nobody");

        assert!(result.is_err(), "unknown assignee should be rejected");
        let store = DataStore::load(&invoker).unwrap();
        assert!(store.find_story(&id).unwrap().assignee.is_none());
    }

    #[test]
    fn assign_story_mutation_resolves_valid_handle_to_member_id() {
        let fixture = TuiFixture::new();
        let invoker = fixture.invoker();
        add_test_member(&invoker, "Mikey-Ward");
        let id = seed_story(&invoker, "Story to assign");

        assign_story_mutation(&invoker, &id, "Mikey-Ward")
            .expect("valid handle should be accepted");

        let store = DataStore::load(&invoker).unwrap();
        assert_eq!(
            store.find_story(&id).unwrap().assignee.as_deref(),
            Some("mikey-ward")
        );
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
        assert_eq!(
            ctx,
            KeyContext::Help,
            "Modal must take priority over filter_bar_focused"
        );
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
        assert_eq!(
            ctx,
            KeyContext::Global,
            "filter_bar_focused should be ignored on Dashboard view"
        );
    }
}
