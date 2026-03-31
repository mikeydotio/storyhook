Now I have complete context on the entire codebase. Here is the architecture design document.

---

# Architecture Design: Storyhook TUI

## System Overview

The TUI is a `story tui` subcommand that launches an interactive terminal application built on ratatui + crossterm. It reuses the existing domain, storage, and output layers directly. All TUI code lives behind a Cargo feature flag (`tui`) so the core CLI binary stays lean.

```
+------------------------------------------------------------------+
|  main.rs                                                         |
|  Detects `story tui` -> enters tui::run() instead of app::run() |
+------------------------------------------------------------------+
         |
         v
+------------------------------------------------------------------+
|  tui/                                                            |
|  +------------------+  +------------------+  +-----------------+ |
|  | event.rs         |  | app.rs           |  | terminal.rs     | |
|  | crossterm events |->| AppState + loop  |->| init/restore    | |
|  | + file watcher   |  | action dispatch  |  |                 | |
|  +------------------+  +------------------+  +-----------------+ |
|                              |                                   |
|         +--------------------+--------------------+              |
|         v                    v                    v              |
|  +-------------+  +------------------+  +------------------+    |
|  | components/ |  | data.rs          |  | theme.rs         |    |
|  | dashboard   |  | DataStore bridge |  | colors, styles   |    |
|  | board       |  | storage -> state |  |                  |    |
|  | detail      |  +------------------+  +------------------+    |
|  | filter_bar  |                                                 |
|  | create_form |                                                 |
|  | status_bar  |                                                 |
|  | help        |                                                 |
|  +-------------+                                                 |
+------------------------------------------------------------------+
         |
         v
+------------------------------------------------------------------+
|  Existing storyhook crate (reused, not modified)                 |
|  domain.rs | storage.rs | output.rs | lock.rs | error.rs         |
+------------------------------------------------------------------+
```

## 1. Module Structure

### File layout

```
src/
  main.rs                    # Modified: detect "tui" subcommand
  lib.rs                     # Modified: add `pub mod tui;` behind feature
  cli.rs                     # Modified: add Invocation::Tui variant
  tui/
    mod.rs                   # Public entry point: `pub fn run(root: &Path) -> Result<()>`
    app.rs                   # TuiApp struct, main loop, action dispatch
    action.rs                # Action enum (all mutations)
    state.rs                 # AppState struct (all shared state)
    event.rs                 # EventSource: crossterm polling + file watcher
    terminal.rs              # Terminal init/restore helpers
    theme.rs                 # Color palette, Style constants
    data.rs                  # DataStore: bridge between storage.rs and TUI state
    keymap.rs                # Key -> Action mapping tables
    focus.rs                 # FocusTarget enum, FocusStack
    components/
      mod.rs                 # Component trait definition
      dashboard.rs           # Dashboard view
      board.rs               # Kanban board view
      story_detail.rs        # Story detail modal
      filter_bar.rs          # Filter bar component
      create_form.rs         # Create story form modal
      status_bar.rs          # Bottom status bar
      help.rs                # Help overlay (keybinding reference)
      card.rs                # Story card rendering (shared by board)
```

### Feature flag in Cargo.toml

```toml
[features]
default = []
tui = ["dep:ratatui", "dep:crossterm", "dep:tokio", "dep:tui-textarea",
       "dep:tui-input", "dep:ratatui-macros", "dep:tui-scrollview",
       "dep:notify"]

[dependencies]
# TUI dependencies (optional, behind feature flag)
ratatui = { version = "0.30", optional = true, default-features = false, features = ["crossterm"] }
crossterm = { version = "0.28", optional = true }
tokio = { version = "1", optional = true, features = ["rt", "macros", "sync", "time"] }
tui-textarea = { version = "0.7", optional = true }
tui-input = { version = "0.11", optional = true }
ratatui-macros = { version = "0.6", optional = true }
tui-scrollview = { version = "0.6", optional = true }
notify = { version = "7", optional = true }
```

### Integration into main.rs

```rust
// In main.rs, before the existing invocation parsing:
if raw_args.first().map(|s| s.as_str()) == Some("tui") {
    #[cfg(feature = "tui")]
    {
        let cwd = env::current_dir().unwrap_or_else(|e| {
            eprintln!("error: failed to resolve current directory: {e}");
            process::exit(1);
        });
        if let Err(e) = storyhook::tui::run(&cwd) {
            eprintln!("error: {e}");
            process::exit(1);
        }
        return;
    }
    #[cfg(not(feature = "tui"))]
    {
        eprintln!("error: TUI not available. Rebuild with --features tui");
        process::exit(1);
    }
}
```

### Why this structure

- **Feature flag** keeps the core binary at ~2MB instead of ~8MB. Users who only want the CLI (especially AI agents) never pay for ratatui, crossterm, tokio.
- **Early intercept in main.rs** rather than routing through `cli::parse_invocation` because the TUI takes over the terminal completely -- it does not produce a `Response` to print.
- **Flat module structure under `tui/`** rather than deeply nested. Seven component files plus seven infrastructure files is manageable. No sub-sub-modules needed at this scale.

## 2. Component Architecture

### The Component Trait

```rust
// src/tui/components/mod.rs

use ratatui::Frame;
use ratatui::layout::Rect;
use crossterm::event::{KeyEvent, MouseEvent};

use super::action::Action;
use super::state::AppState;

/// Every visual component implements this trait.
/// Components are NOT stateless renderers -- they own local UI state
/// (cursor position, scroll offset, which field is focused in a form).
/// They do NOT own domain data. Domain data lives in AppState.
pub trait Component {
    /// Handle a key event. Return actions to dispatch to AppState.
    /// Return an empty vec to swallow the event.
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action>;

    /// Handle a mouse event. Return actions to dispatch.
    fn handle_mouse(&mut self, mouse: MouseEvent, state: &AppState) -> Vec<Action>;

    /// Render into the given area. Read from AppState, never mutate it.
    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState);

    /// Called after AppState changes so the component can adjust local
    /// state (e.g., clamp scroll offset after data reload).
    fn on_state_change(&mut self, state: &AppState) {}

    /// Return the hit-test regions this component registered during
    /// the last render. Used for mouse click routing.
    fn hit_regions(&self) -> &[HitRegion] {
        &[]
    }
}

/// A clickable/draggable region tracked after rendering.
#[derive(Clone, Debug)]
pub struct HitRegion {
    pub rect: Rect,
    pub target: HitTarget,
}

#[derive(Clone, Debug)]
pub enum HitTarget {
    StoryCard { id: String, column_index: usize },
    Column { index: usize },
    FilterChip { index: usize },
    DashboardSection { name: String },
    Button { id: String },
}
```

### Component Breakdown

#### Dashboard (`dashboard.rs`)

- **Purpose:** Home screen showing project summary, metrics, and navigation.
- **Local state:**
  ```rust
  pub struct Dashboard {
      selected_section: usize,  // 0=summary, 1=by-state, 2=ready, 3=recent
      scroll_offset: u16,
  }
  ```
- **Events handled:** `j`/`k`/Up/Down to select section, `Enter` to navigate (board), `r` to refresh.
- **Actions emitted:** `Action::SwitchView(View::Board)`, `Action::RefreshData`, `Action::ShowStory(id)`.
- **Renders:** Summary stats (total, open, closed, blocked, ready), state distribution bar, priority breakdown, top 5 ready stories list, recent activity.

#### Board (`board.rs`)

- **Purpose:** Kanban board with one column per state from `states.toml`.
- **Local state:**
  ```rust
  pub struct Board {
      selected_column: usize,
      selected_row: usize,       // row within column
      column_scroll: Vec<u16>,   // per-column vertical scroll offset
      horizontal_scroll: u16,    // if columns exceed terminal width
      hit_regions: Vec<HitRegion>,
      drag_state: DragState,
  }

  pub enum DragState {
      Idle,
      Pending { story_id: String, origin_col: usize, start_pos: (u16, u16) },
      Dragging { story_id: String, origin_col: usize, current_col: usize },
  }
  ```
- **Events handled:** `h`/`l`/Left/Right to move between columns, `j`/`k`/Up/Down to move within a column, `>`/`<` or `H`/`L` to move a story to adjacent state, `Enter` to open detail, `n` to create, `/` to focus filter bar, mouse clicks for card selection, mouse scroll for column scroll.
- **Actions emitted:** `Action::SelectStory(id)`, `Action::MoveStory { id, target_state }`, `Action::OpenDetail(id)`, `Action::OpenCreateForm`, `Action::FocusFilterBar`.
- **Renders:** Column headers with state name and count, story cards in each column, selection highlight.

#### StoryDetail (`story_detail.rs`)

- **Purpose:** Modal overlay (~66% screen) for viewing and inline editing a story.
- **Local state:**
  ```rust
  pub struct StoryDetail {
      mode: DetailMode,
      scroll_offset: u16,
      edit_field: Option<EditField>,
      title_input: tui_input::Input,
      comment_textarea: tui_textarea::TextArea<'static>,
      priority_cursor: usize,
      label_input: tui_input::Input,
      assignee_input: tui_input::Input,
  }

  pub enum DetailMode {
      Viewing,
      EditingTitle,
      EditingPriority,
      EditingLabels,
      EditingAssignee,
      AddingComment,
  }

  pub enum EditField {
      Title,
      Priority,
      Labels,
      Assignee,
      Comment,
  }
  ```
- **Events handled:** `Esc` to close (or cancel edit), `e` to enter edit mode on selected field, `Tab`/`Shift+Tab` to cycle fields during edit, `Enter` to confirm edit, `c` to add comment, `Ctrl+E` to open `$EDITOR`, `>`/`<` to change state inline.
- **Actions emitted:** `Action::UpdateTitle { id, title }`, `Action::MoveStory { id, target_state }`, `Action::SetPriority { id, priority }`, `Action::SetLabels { id, labels }`, `Action::AssignStory { id, assignee }`, `Action::AddComment { id, text }`, `Action::CloseModal`.
- **Renders:** Title, state (with change buttons), priority, assignee, labels, relationships, comments (scrollable), edit widgets when in edit mode.

#### FilterBar (`filter_bar.rs`)

- **Purpose:** Persistent filter bar above the board. Shows active filters and accepts new filter input.
- **Local state:**
  ```rust
  pub struct FilterBar {
      input: tui_input::Input,
      focused: bool,
      suggestions: Vec<String>,
      suggestion_cursor: usize,
  }
  ```
- **Events handled (when focused):** Text input for search/filter, `Tab` to accept suggestion, `Enter` to apply filter, `Backspace` on empty input to remove last filter chip, `Esc` to unfocus.
- **Actions emitted:** `Action::SetFilter(FilterSpec)`, `Action::ClearFilter(index)`, `Action::ClearAllFilters`, `Action::UnfocusFilterBar`.
- **Renders:** Active filter chips (colored by type), text input cursor, suggestion dropdown.

#### CreateForm (`create_form.rs`)

- **Purpose:** Modal form for creating a new story.
- **Local state:**
  ```rust
  pub struct CreateForm {
      title_input: tui_input::Input,
      priority_cursor: usize,
      label_input: tui_input::Input,
      assignee_input: tui_input::Input,
      focused_field: usize,  // 0=title, 1=priority, 2=labels, 3=assignee
  }
  ```
- **Events handled:** `Tab`/`Shift+Tab` to cycle fields, `Enter` on title field creates the story (if title non-empty), `Esc` to cancel.
- **Actions emitted:** `Action::CreateStory { title, priority, labels, assignee }`, `Action::CloseModal`.
- **Renders:** Form fields with labels, focused field highlighted, submit/cancel hints.

#### StatusBar (`status_bar.rs`)

- **Purpose:** Bottom bar showing context-sensitive key hints, current view name, and notification messages.
- **Local state:**
  ```rust
  pub struct StatusBar {
      notification: Option<(String, std::time::Instant)>,
  }
  ```
- **Events handled:** None directly (passive component).
- **Actions emitted:** None.
- **Renders:** Left: context-sensitive shortcuts for current view. Center: notification message (fades after 3s). Right: current view label, story count.

#### Help (`help.rs`)

- **Purpose:** Full-screen keybinding reference overlay.
- **Local state:**
  ```rust
  pub struct Help {
      scroll_offset: u16,
  }
  ```
- **Events handled:** `j`/`k`/Up/Down to scroll, `Esc`/`q`/`?` to close.
- **Actions emitted:** `Action::CloseModal`.
- **Renders:** Two-column table of all keybindings, grouped by context.

## 3. Data Flow

### DataStore: The Bridge

```rust
// src/tui/data.rs

use std::path::Path;
use std::collections::BTreeMap;

use crate::domain::{StateDef, StorySnapshot, Priority, SuperState};
use crate::storage;
use crate::error::AppError;

/// Read-only snapshot of all project data the TUI needs.
/// Rebuilt on every refresh. Cheap to clone (stories are ~100 bytes each
/// plus strings -- even 500 stories is < 1MB).
#[derive(Clone, Debug)]
pub struct DataStore {
    pub states: Vec<StateDef>,
    pub state_map: BTreeMap<String, StateDef>,
    pub stories: Vec<StorySnapshot>,
    pub prefix: String,
    pub members: Vec<crate::domain::Member>,
}

impl DataStore {
    /// Load everything from disk. This is the ONLY place the TUI reads
    /// from storage.rs. Called on startup and on every refresh.
    pub fn load(root: &Path) -> Result<Self, AppError> {
        storage::ensure_project(root)?;
        let states = storage::load_states(root)?;
        let state_map = storage::load_state_map(root)?;
        let stories = storage::load_all_open_snapshots(root)?;
        let prefix = storage::load_project_prefix(root)?;
        let members = storage::load_members(root)?;
        Ok(Self { states, state_map, stories, prefix, members })
    }

    /// Stories grouped by state slug, in state definition order.
    pub fn stories_by_state(&self) -> Vec<(&StateDef, Vec<&StorySnapshot>)> {
        self.states
            .iter()
            .filter(|s| s.super_state == SuperState::Open)
            .map(|state_def| {
                let stories: Vec<&StorySnapshot> = self.stories
                    .iter()
                    .filter(|s| s.state == state_def.slug)
                    .collect();
                (state_def, stories)
            })
            .collect()
    }

    pub fn find_story(&self, id: &str) -> Option<&StorySnapshot> {
        self.stories.iter().find(|s| s.id == id)
    }
}
```

### Refresh Strategy

```
                  +-------------+
                  | notify crate|  watches .storyhook/open/stories/
                  +------+------+
                         |
                    file change
                         |
                         v
                  +------+------+
                  | EventSource |  debounces 200ms, sends Event::DataChanged
                  +------+------+
                         |
                         v
                  +------+------+
                  |  Main Loop  |  dispatches Action::RefreshData
                  +------+------+
                         |
                         v
                  +------+------+
                  | DataStore   |  re-reads all open stories from disk
                  +------+------+  (typically < 5ms for ~100 stories)
                         |
                         v
                  +------+------+
                  | AppState    |  replaces data field, notifies components
                  +------+------+
```

File watching uses the `notify` crate watching `.storyhook/open/stories/`. Events are debounced to 200ms to avoid re-reading during rapid CLI operations (e.g., `story decompose` creating many stories). Additionally:
- Manual refresh with `r` key always works.
- `Action::RefreshData` is also emitted after any mutation (create, move, edit) as a confirmation re-read.

### Write Path

All mutations go through existing `storage.rs` functions, wrapped in `lock::with_project_lock`. The TUI never writes directly to files. This preserves the same concurrency safety the CLI has.

```rust
// In action dispatch (app.rs):
Action::MoveStory { ref id, ref target_state } => {
    lock::with_project_lock(&self.root, || {
        let events = storage::load_open_story_events(&self.root, id)?;
        let mut new_events = events.clone();
        new_events.push(StoryEvent::StoryStateChanged {
            at: storage::now(),
            state: target_state.clone(),
        });
        storage::rewrite_story_events(&self.root, id, &new_events)?;
        Ok(())
    })?;
    // Auto-archive if moved to a CLOSED state
    if let Some(state_def) = self.state.data.state_map.get(target_state) {
        if state_def.super_state == SuperState::Closed {
            storage::archive_story(&self.root, id)?;
        }
    }
    self.refresh_data()?;
}
```

## 4. Event Handling

### EventSource

```rust
// src/tui/event.rs

use std::time::Duration;
use tokio::sync::mpsc;
use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, MouseEvent};

pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    DataChanged,    // from file watcher
    Tick,           // 250ms tick for animations/status bar
}

pub struct EventSource {
    rx: mpsc::UnboundedReceiver<Event>,
}

impl EventSource {
    pub fn new(root: &std::path::Path) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        // Spawn crossterm event polling thread
        let tx_input = tx.clone();
        std::thread::spawn(move || {
            loop {
                if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                    if let Ok(evt) = event::read() {
                        let mapped = match evt {
                            CrosstermEvent::Key(k) => Some(Event::Key(k)),
                            CrosstermEvent::Mouse(m) => Some(Event::Mouse(m)),
                            CrosstermEvent::Resize(w, h) => Some(Event::Resize(w, h)),
                            _ => None,
                        };
                        if let Some(e) = mapped {
                            if tx_input.send(e).is_err() { break; }
                        }
                    }
                }
            }
        });

        // Spawn file watcher thread
        let tx_watch = tx.clone();
        let watch_path = root.join(".storyhook/open/stories");
        std::thread::spawn(move || {
            use notify::{Watcher, RecursiveMode, recommended_watcher};
            let (ntx, nrx) = std::sync::mpsc::channel();
            let mut watcher = recommended_watcher(ntx).unwrap();
            let _ = watcher.watch(&watch_path, RecursiveMode::NonRecursive);
            let mut last_event = std::time::Instant::now();
            for _event in nrx {
                // Debounce: skip if < 200ms since last forwarded event
                if last_event.elapsed() > Duration::from_millis(200) {
                    let _ = tx_watch.send(Event::DataChanged);
                    last_event = std::time::Instant::now();
                }
            }
        });

        // Spawn tick thread
        let tx_tick = tx;
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if tx_tick.send(Event::Tick).is_err() { break; }
            }
        });

        Self { rx }
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}
```

### Focus Management

```rust
// src/tui/focus.rs

/// What currently has keyboard focus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Dashboard,
    Board,
    FilterBar,
}

/// Modal overlay stack. Modals trap focus entirely.
/// When the stack is non-empty, the top modal gets all events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    StoryDetail { story_id: String },
    CreateForm,
    Help,
}

pub struct FocusStack {
    pub base: FocusTarget,
    pub modals: Vec<Modal>,
}

impl FocusStack {
    pub fn new(base: FocusTarget) -> Self {
        Self { base, modals: Vec::new() }
    }

    pub fn push_modal(&mut self, modal: Modal) {
        self.modals.push(modal);
    }

    pub fn pop_modal(&mut self) -> Option<Modal> {
        self.modals.pop()
    }

    pub fn active_modal(&self) -> Option<&Modal> {
        self.modals.last()
    }

    pub fn has_modal(&self) -> bool {
        !self.modals.is_empty()
    }
}
```

### Event Routing in the Main Loop

```rust
// Pseudocode for event routing in app.rs

match event {
    Event::Key(key) => {
        // Global keys that work regardless of focus
        if key == KeyCode::Char('q') && !self.focus.has_modal() && !self.filter_bar.focused {
            return Ok(true); // signal quit
        }

        // Modal focus trapping: if a modal is open, only it gets keys
        if let Some(modal) = self.focus.active_modal() {
            let actions = match modal {
                Modal::StoryDetail { .. } => self.story_detail.handle_key(key, &self.state),
                Modal::CreateForm => self.create_form.handle_key(key, &self.state),
                Modal::Help => self.help.handle_key(key, &self.state),
            };
            self.dispatch_actions(actions)?;
            return Ok(false);
        }

        // Base focus routing
        let actions = match self.focus.base {
            FocusTarget::Dashboard => self.dashboard.handle_key(key, &self.state),
            FocusTarget::Board => {
                if self.filter_bar.focused {
                    self.filter_bar.handle_key(key, &self.state)
                } else {
                    self.board.handle_key(key, &self.state)
                }
            }
            FocusTarget::FilterBar => self.filter_bar.handle_key(key, &self.state),
        };
        self.dispatch_actions(actions)?;
    }

    Event::Mouse(mouse) => {
        // Mouse events route via hit-testing, ignoring focus stack
        // (except modals still trap mouse clicks within their area)
        // ...
    }

    Event::DataChanged => {
        self.dispatch_actions(vec![Action::RefreshData])?;
    }

    Event::Resize(w, h) => {
        // ratatui handles this automatically on next render
    }

    Event::Tick => {
        // Clear expired notifications
        self.status_bar.clear_expired();
    }
}
```

## 5. Keyboard Shortcuts

### Global (always active, no modal open)

| Key | Action |
|-----|--------|
| `q` | Quit (unless text input focused) |
| `Ctrl+C` | Force quit |
| `?` | Toggle help overlay |
| `1` | Switch to Dashboard view |
| `2` | Switch to Board view |
| `r` | Refresh data from disk |

### Dashboard View

| Key | Action |
|-----|--------|
| `j` / Down | Select next section/item |
| `k` / Up | Select previous section/item |
| `Enter` | Navigate to selected (board, story) |
| `n` | Open create story form |

### Board View

| Key | Action |
|-----|--------|
| `h` / Left | Move selection to left column |
| `l` / Right | Move selection to right column |
| `j` / Down | Move selection down within column |
| `k` / Up | Move selection up within column |
| `g` | Jump to first card in column |
| `G` | Jump to last card in column |
| `Enter` | Open story detail modal |
| `n` | Open create story form |
| `/` | Focus filter bar |
| `>` / `L` | Move selected story to next state (right) |
| `<` / `H` | Move selected story to previous state (left) |
| `Space` | Toggle story selection (for future batch ops) |
| `Tab` | Cycle to next column |
| `Shift+Tab` | Cycle to previous column |

### Filter Bar (when focused)

| Key | Action |
|-----|--------|
| Text input | Type filter query |
| `Enter` | Apply filter |
| `Tab` | Accept autocomplete suggestion |
| `Backspace` (empty) | Remove last filter chip |
| `Esc` | Unfocus filter bar, return to board |
| `Ctrl+U` | Clear all filters |

### Story Detail Modal

| Key | Action |
|-----|--------|
| `Esc` | Close modal (or cancel current edit) |
| `e` | Edit selected field |
| `j` / Down | Select next field |
| `k` / Up | Select previous field |
| `>` | Move story to next state |
| `<` | Move story to previous state |
| `c` | Add comment |
| `Ctrl+E` | Open `$EDITOR` for long text |
| `Enter` | Confirm edit |
| `Tab` | Next field (during edit) |
| `Shift+Tab` | Previous field (during edit) |

### Create Form Modal

| Key | Action |
|-----|--------|
| `Esc` | Cancel and close |
| `Tab` | Next field |
| `Shift+Tab` | Previous field |
| `Enter` | Submit (when on last field) or next field |
| `Ctrl+Enter` | Submit from any field |

### Help Overlay

| Key | Action |
|-----|--------|
| `Esc` / `q` / `?` | Close help |
| `j` / Down | Scroll down |
| `k` / Up | Scroll up |

## 6. Mouse Interaction

### Phase 1 (Initial Release): Click and Scroll

| Interaction | Behavior |
|-------------|----------|
| Left-click on card | Select that card, highlight it |
| Double-click on card | Open story detail modal |
| Left-click on column header | Select that column |
| Left-click on filter chip | Remove that filter |
| Left-click on filter bar | Focus filter bar |
| Scroll wheel in column | Scroll that column vertically |
| Scroll wheel on board | Horizontal scroll if columns overflow |
| Left-click on dashboard item | Select/navigate |
| Left-click outside modal | Close modal |

Mouse click routing works through hit-testing. Each component records `HitRegion` entries during `render()` (the Rects of cards, columns, buttons). On mouse event, we iterate regions to find the target.

```rust
fn route_mouse(&mut self, mouse: MouseEvent) -> Vec<Action> {
    let pos = (mouse.column, mouse.row);

    // If modal is open, clicks outside close it
    if self.focus.has_modal() {
        if !self.modal_rect.contains(pos) {
            return vec![Action::CloseModal];
        }
        // Route to modal component
        return match self.focus.active_modal().unwrap() {
            Modal::StoryDetail { .. } => self.story_detail.handle_mouse(mouse, &self.state),
            Modal::CreateForm => self.create_form.handle_mouse(mouse, &self.state),
            Modal::Help => self.help.handle_mouse(mouse, &self.state),
        };
    }

    // Hit-test board regions
    for region in self.board.hit_regions() {
        if region.rect.contains(pos) {
            match &region.target {
                HitTarget::StoryCard { id, .. } => {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        return vec![Action::SelectStory(id.clone())];
                    }
                }
                // ... other targets
            }
        }
    }

    vec![]
}
```

### Phase 2 (Deferred): Drag-and-Drop

The drag state machine is defined but initially stubbed out. The `Board` component's `DragState` enum already accounts for it:

```
                   MouseDown on card
          Idle -----------------------> Pending
           ^                              |
           |                     +--------+--------+
           |                     |                  |
           |              MouseDrag            MouseUp (no drag)
           |              (> 3 cells)          => Click/Select
           |                     |
           |                     v
           |                  Dragging
           |                     |
           +-----+------+-------+
                 |      |
             MouseUp   Esc
             => Drop   => Cancel
```

Visual feedback during drag:
- Source card rendered with dimmed style
- Target column border changes to accent color
- Insertion indicator (horizontal line) shown at drop position
- No floating ghost (terminal limitation)

## 7. Rendering

### Main Layout

```
+------------------------------------------------------------------+
| [1] Dashboard  [2] Board            storyhook v0.6.0  SH-  13 open|  <- Tab bar (1 line)
+------------------------------------------------------------------+
| /search  [priority:high] [state:todo] [x clear all]              |  <- Filter bar (1 line, board only)
+------------------------------------------------------------------+
|                                                                    |
|  todo (5)          | in-progress (3)  | done (2)                  |
|  +--------------+  | +--------------+ | +--------------+          |
|  | SH-12        |  | | SH-8         | | | SH-3         |          |  <- Board area
|  | Fix login    |  | | Add search   | | | Init project |          |     (fills remaining)
|  | !! high      |  | | ! med        | | |              |          |
|  | [bug]        |  | | @mikey       | | |              |          |
|  +--------------+  | +--------------+ | +--------------+          |
|  | SH-14        |  | | SH-11        | |                          |
|  | Refactor DB  |  | | TUI board    | |                          |
|  | . low        |  | | !!! crit     | |                          |
|  |              |  | | [tui] @mikey | |                          |
|  +--------------+  | +--------------+ |                          |
|  | ...          |  |                  |                          |
|                    |                  |                          |
+------------------------------------------------------------------+
| q:quit  ?:help  n:new  /:filter  Enter:detail  >/<:move state   |  <- Status bar (1 line)
+------------------------------------------------------------------+
```

### Layout Calculations

```rust
// In app.rs render method:
fn render(&self, frame: &mut Frame) {
    let area = frame.area();

    // Main vertical layout
    let [tab_bar, content, status_bar] = Layout::vertical([
        Constraint::Length(1),    // tab bar
        Constraint::Min(10),     // content area
        Constraint::Length(1),    // status bar
    ]).areas(area);

    // Render tab bar
    self.render_tab_bar(frame, tab_bar);

    // Content depends on current view
    match self.focus.base {
        FocusTarget::Dashboard => {
            self.dashboard.render(frame, content, &self.state);
        }
        FocusTarget::Board => {
            // Board view has filter bar + board
            let [filter_area, board_area] = Layout::vertical([
                Constraint::Length(1),   // filter bar
                Constraint::Min(8),      // board
            ]).areas(content);

            self.filter_bar.render(frame, filter_area, &self.state);
            self.board.render(frame, board_area, &self.state);
        }
        _ => {}
    }

    // Status bar (always visible)
    self.status_bar.render(frame, status_bar, &self.state);

    // Modal overlay (rendered last = on top)
    if let Some(modal) = self.focus.active_modal() {
        let modal_rect = centered_rect(area, 66, 75);
        frame.render_widget(Clear, modal_rect);

        match modal {
            Modal::StoryDetail { .. } => {
                self.story_detail.render(frame, modal_rect, &self.state);
            }
            Modal::CreateForm => {
                self.create_form.render(frame, modal_rect, &self.state);
            }
            Modal::Help => {
                self.help.render(frame, modal_rect, &self.state);
            }
        }
    }
}

fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let w = area.width * percent_x / 100;
    let h = area.height * percent_y / 100;
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    Rect::new(area.x + x, area.y + y, w, h)
}
```

### Board Column Layout

```rust
// In board.rs render method:

fn render_columns(&self, frame: &mut Frame, area: Rect, state: &AppState) {
    let columns = state.data.stories_by_state();
    let num_columns = columns.len();

    // Minimum column width: 22 chars (ID + short title + border)
    // If all columns fit, equal width. Otherwise, show what fits + scroll.
    let min_col_width: u16 = 22;
    let available = area.width;
    let visible_cols = (available / min_col_width).max(1) as usize;

    let col_width = if num_columns <= visible_cols {
        available / num_columns as u16
    } else {
        min_col_width
    };

    let start_col = self.horizontal_scroll as usize;
    let end_col = (start_col + visible_cols).min(num_columns);

    let constraints: Vec<Constraint> = (start_col..end_col)
        .map(|_| Constraint::Length(col_width))
        .collect();

    let col_areas = Layout::horizontal(constraints).split(area);

    for (i, col_area) in col_areas.iter().enumerate() {
        let col_idx = start_col + i;
        let (state_def, stories) = &columns[col_idx];
        self.render_column(frame, *col_area, col_idx, state_def, stories, state);
    }
}
```

### Story Card Rendering

```
+-----------------------+
| SH-12                 |    <- ID (monospace, dimmed)
| Fix login flow bug    |    <- Title (bold, truncated to fit)
| !! high   [bug]       |    <- Priority indicator + labels
| @mikey    BLOCKED     |    <- Assignee + blocked/awaiting badge
+-----------------------+
```

```rust
// src/tui/components/card.rs

pub fn render_card(
    frame: &mut Frame,
    area: Rect,
    story: &StorySnapshot,
    selected: bool,
    theme: &Theme,
) {
    let border_style = if selected {
        theme.card_selected
    } else {
        theme.card_normal
    };

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Line 1: Story ID
    let id_span = Span::styled(&story.id, theme.card_id);
    frame.render_widget(Paragraph::new(id_span), Rect { height: 1, ..inner });

    // Line 2: Title (truncated)
    let title_area = Rect { y: inner.y + 1, height: 1, ..inner };
    let title_span = Span::styled(&story.title, theme.card_title);
    frame.render_widget(
        Paragraph::new(title_span).wrap(Wrap { trim: true }),
        title_area,
    );

    // Line 3: Priority + labels
    let meta_area = Rect { y: inner.y + 2, height: 1, ..inner };
    let mut meta_spans = Vec::new();
    if story.priority != Priority::None {
        meta_spans.push(priority_span(&story.priority, theme));
        meta_spans.push(Span::raw("  "));
    }
    for label in &story.labels {
        meta_spans.push(Span::styled(
            format!("[{}]", label),
            theme.label,
        ));
        meta_spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(meta_spans)), meta_area);

    // Line 4: Assignee + status badge
    let badge_area = Rect { y: inner.y + 3, height: 1, ..inner };
    let mut badge_spans = Vec::new();
    if let Some(ref assignee) = story.assignee {
        badge_spans.push(Span::styled(
            format!("@{}", assignee),
            theme.assignee,
        ));
    }
    if story.awaiting.is_some() {
        badge_spans.push(Span::styled(" BLOCKED", theme.blocked_badge));
    }
    frame.render_widget(Paragraph::new(Line::from(badge_spans)), badge_area);
}

/// Card height: 2 for border + 4 content lines = 6
pub const CARD_HEIGHT: u16 = 6;
```

### Color Scheme (Theme)

Uses ANSI 16 named colors so the palette respects the user's terminal theme. Honors `$NO_COLOR`.

```rust
// src/tui/theme.rs

use ratatui::style::{Color, Modifier, Style};

pub struct Theme {
    // Card styles
    pub card_normal: Style,
    pub card_selected: Style,
    pub card_id: Style,
    pub card_title: Style,

    // Priority
    pub priority_critical: Style,
    pub priority_high: Style,
    pub priority_medium: Style,
    pub priority_low: Style,

    // Metadata
    pub label: Style,
    pub assignee: Style,
    pub blocked_badge: Style,

    // Column
    pub column_header: Style,
    pub column_header_count: Style,
    pub column_border: Style,

    // Filter bar
    pub filter_chip: Style,
    pub filter_input: Style,

    // Status bar
    pub status_bar: Style,
    pub status_key: Style,

    // Tab bar
    pub tab_active: Style,
    pub tab_inactive: Style,

    // Modal
    pub modal_border: Style,
    pub modal_title: Style,
}

impl Theme {
    pub fn default_ansi() -> Self {
        let no_color = std::env::var("NO_COLOR").is_ok();

        if no_color {
            return Self::no_color();
        }

        Self {
            card_normal: Style::default().fg(Color::White),
            card_selected: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            card_id: Style::default().fg(Color::DarkGray),
            card_title: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),

            priority_critical: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            priority_high: Style::default().fg(Color::Yellow),
            priority_medium: Style::default().fg(Color::Blue),
            priority_low: Style::default().fg(Color::DarkGray),

            label: Style::default().fg(Color::Cyan),
            assignee: Style::default().fg(Color::Green),
            blocked_badge: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),

            column_header: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            column_header_count: Style::default().fg(Color::DarkGray),
            column_border: Style::default().fg(Color::DarkGray),

            filter_chip: Style::default().fg(Color::Black).bg(Color::Cyan),
            filter_input: Style::default().fg(Color::White),

            status_bar: Style::default().fg(Color::DarkGray),
            status_key: Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),

            tab_active: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            tab_inactive: Style::default().fg(Color::DarkGray),

            modal_border: Style::default().fg(Color::Cyan),
            modal_title: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        }
    }

    fn no_color() -> Self {
        let plain = Style::default();
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let underline = Style::default().add_modifier(Modifier::UNDERLINED);

        Self {
            card_normal: plain,
            card_selected: bold,
            card_id: dim,
            card_title: bold,
            priority_critical: bold,
            priority_high: bold,
            priority_medium: plain,
            priority_low: dim,
            label: plain,
            assignee: plain,
            blocked_badge: bold,
            column_header: bold,
            column_header_count: dim,
            column_border: dim,
            filter_chip: bold,
            filter_input: plain,
            status_bar: dim,
            status_key: bold,
            tab_active: underline,
            tab_inactive: dim,
            modal_border: plain,
            modal_title: bold,
        }
    }
}
```

Priority indicators (text, not emoji):

| Priority | Symbol | Color |
|----------|--------|-------|
| Critical | `!!!` | Red bold |
| High | `!!` | Yellow |
| Medium | `!` | Blue |
| Low | `.` | DarkGray |
| None | (none) | (omitted) |

## 8. Integration Points

### Reused from Existing Codebase (No Modifications)

| Module | What the TUI uses |
|--------|-------------------|
| `domain.rs` | `StorySnapshot`, `StoryEvent`, `StateDef`, `Priority`, `SuperState`, `Member`, `StoryRelation`, `StoryComment`, `fold_story`, `is_ready`, `is_relation_input` |
| `storage.rs` | `ProjectPaths`, `load_states`, `load_state_map`, `load_all_open_snapshots`, `load_open_story_events`, `load_open_story_snapshot`, `load_members`, `load_project_prefix`, `create_story`, `write_story_events`, `rewrite_story_events`, `archive_story`, `ensure_project`, `now` |
| `output.rs` | `StoryView`, `SummaryView`, `StaleInfo` (data structures only -- the TUI does its own rendering) |
| `lock.rs` | `with_project_lock` (for all mutations) |
| `error.rs` | `AppError` (displayed in status bar notifications) |

### New Code vs Reused

| Concern | New TUI code | Reused existing code |
|---------|-------------|---------------------|
| Data loading | `DataStore` (thin wrapper) | `storage::load_*` functions |
| Data mutation | Action dispatch in `app.rs` | `storage::write_*`, `storage::create_*` |
| Story filtering | `FilterSpec` matching logic | `domain::is_ready`, `Priority::parse` |
| State definitions | Column ordering from `states` | `storage::load_states` |
| File locking | (delegates) | `lock::with_project_lock` |
| Summary computation | Dashboard metrics | Could reuse `app.rs` summary logic, but it couples to `Response` -- cleaner to re-derive from `DataStore.stories` |
| Rendering | All new (ratatui widgets) | Nothing -- existing `output.rs` is CLI-specific |

### Modified Existing Files

Only three existing files need changes, all minimal:

1. **`Cargo.toml`** -- Add optional TUI dependencies and `[features]` section.
2. **`src/lib.rs`** -- Add `#[cfg(feature = "tui")] pub mod tui;`
3. **`src/main.rs`** -- Add early intercept for `story tui` subcommand (the `cli.rs` parser does not need a new `Invocation` variant since we intercept before parsing).

## 9. Interface Definitions

### Action Enum (Complete)

```rust
// src/tui/action.rs

use crate::domain::Priority;

#[derive(Clone, Debug)]
pub enum Action {
    // Navigation
    SwitchView(View),
    OpenDetail(String),          // story ID
    OpenCreateForm,
    CloseModal,
    FocusFilterBar,
    UnfocusFilterBar,
    ToggleHelp,

    // Data mutations (write to disk via storage.rs)
    CreateStory {
        title: String,
        priority: Option<Priority>,
        labels: Vec<String>,
        assignee: Option<String>,
    },
    MoveStory {
        id: String,
        target_state: String,
    },
    UpdateTitle {
        id: String,
        title: String,
    },
    SetPriority {
        id: String,
        priority: Priority,
    },
    SetLabels {
        id: String,
        labels: Vec<String>,
    },
    AssignStory {
        id: String,
        assignee: String,
    },
    AddComment {
        id: String,
        text: String,
    },
    SetAwaiting {
        id: String,
        reason: String,
    },
    ClearAwaiting {
        id: String,
    },

    // Filtering
    SetFilter(FilterSpec),
    ClearFilter(usize),          // index
    ClearAllFilters,

    // Selection
    SelectStory(String),         // story ID

    // Data refresh
    RefreshData,

    // Notifications
    Notify(String),

    // Quit
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Board,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterSpec {
    Search(String),
    State(String),
    Priority(Priority),
    Label(String),
    Assignee(String),
    Blocked,
    Ready,
}
```

### AppState Struct

```rust
// src/tui/state.rs

use super::action::{FilterSpec, View};
use super::data::DataStore;
use super::focus::{FocusStack, FocusTarget};
use super::theme::Theme;
use crate::domain::StorySnapshot;

pub struct AppState {
    pub data: DataStore,
    pub filters: Vec<FilterSpec>,
    pub selected_story_id: Option<String>,
    pub theme: Theme,
    pub terminal_size: (u16, u16),
}

impl AppState {
    /// Return stories filtered by current active filters,
    /// sorted by priority then creation date.
    pub fn filtered_stories(&self) -> Vec<&StorySnapshot> {
        let mut stories: Vec<&StorySnapshot> = self.data.stories.iter()
            .filter(|s| self.matches_filters(s))
            .collect();
        stories.sort_by(|a, b| {
            a.priority.cmp(&b.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        stories
    }

    fn matches_filters(&self, story: &StorySnapshot) -> bool {
        self.filters.iter().all(|filter| match filter {
            FilterSpec::Search(query) => {
                let q = query.to_lowercase();
                story.title.to_lowercase().contains(&q)
                    || story.id.to_lowercase().contains(&q)
                    || story.labels.iter().any(|l| l.to_lowercase().contains(&q))
            }
            FilterSpec::State(state) => story.state == *state,
            FilterSpec::Priority(p) => story.priority == *p,
            FilterSpec::Label(label) => story.labels.contains(label),
            FilterSpec::Assignee(a) => story.assignee.as_deref() == Some(a.as_str()),
            FilterSpec::Blocked => story.awaiting.is_some(),
            FilterSpec::Ready => {
                story.awaiting.is_none()
                    && story.superstate == crate::domain::SuperState::Open
            }
        })
    }
}
```

### TuiApp (Main Application Struct)

```rust
// src/tui/app.rs

use std::path::{Path, PathBuf};

use super::action::{Action, View};
use super::components::*;
use super::data::DataStore;
use super::event::{Event, EventSource};
use super::focus::{FocusStack, FocusTarget, Modal};
use super::state::AppState;
use super::theme::Theme;
use crate::error::AppError;

pub struct TuiApp {
    root: PathBuf,
    state: AppState,
    focus: FocusStack,

    // Components
    dashboard: Dashboard,
    board: Board,
    filter_bar: FilterBar,
    story_detail: StoryDetail,
    create_form: CreateForm,
    status_bar: StatusBar,
    help: Help,
}

impl TuiApp {
    pub fn new(root: &Path) -> Result<Self, AppError> {
        let data = DataStore::load(root)?;
        let theme = Theme::default_ansi();
        let num_columns = data.states.iter()
            .filter(|s| s.super_state == crate::domain::SuperState::Open)
            .count();

        Ok(Self {
            root: root.to_path_buf(),
            state: AppState {
                data,
                filters: Vec::new(),
                selected_story_id: None,
                theme,
                terminal_size: (80, 24), // updated on first render
            },
            focus: FocusStack::new(FocusTarget::Dashboard),

            dashboard: Dashboard::new(),
            board: Board::new(num_columns),
            filter_bar: FilterBar::new(),
            story_detail: StoryDetail::new(),
            create_form: CreateForm::new(),
            status_bar: StatusBar::new(),
            help: Help::new(),
        })
    }

    pub async fn run(&mut self) -> Result<(), AppError> {
        let mut terminal = ratatui::init();
        crossterm::execute!(
            std::io::stdout(),
            crossterm::event::EnableMouseCapture
        ).map_err(|e| AppError::Storage(e.to_string()))?;

        let mut events = EventSource::new(&self.root);

        loop {
            // Render
            terminal.draw(|frame| {
                self.state.terminal_size = (frame.area().width, frame.area().height);
                self.render(frame);
            }).map_err(|e| AppError::Storage(e.to_string()))?;

            // Wait for next event
            if let Some(event) = events.next().await {
                let actions = self.handle_event(event);
                for action in actions {
                    if matches!(action, Action::Quit) {
                        crossterm::execute!(
                            std::io::stdout(),
                            crossterm::event::DisableMouseCapture
                        ).ok();
                        ratatui::restore();
                        return Ok(());
                    }
                    self.dispatch(action)?;
                }
            }
        }
    }

    fn dispatch(&mut self, action: Action) -> Result<(), AppError> {
        match action {
            Action::SwitchView(view) => {
                self.focus.base = match view {
                    View::Dashboard => FocusTarget::Dashboard,
                    View::Board => FocusTarget::Board,
                };
            }
            Action::OpenDetail(id) => {
                self.story_detail.load_story(&id, &self.state);
                self.focus.push_modal(Modal::StoryDetail { story_id: id });
            }
            Action::OpenCreateForm => {
                self.create_form.reset();
                self.focus.push_modal(Modal::CreateForm);
            }
            Action::CloseModal => {
                self.focus.pop_modal();
            }
            Action::ToggleHelp => {
                if matches!(self.focus.active_modal(), Some(Modal::Help)) {
                    self.focus.pop_modal();
                } else {
                    self.focus.push_modal(Modal::Help);
                }
            }
            Action::FocusFilterBar => {
                self.filter_bar.focused = true;
            }
            Action::UnfocusFilterBar => {
                self.filter_bar.focused = false;
            }
            Action::RefreshData => {
                self.state.data = DataStore::load(&self.root)?;
                // Notify all components
                self.dashboard.on_state_change(&self.state);
                self.board.on_state_change(&self.state);
            }
            Action::CreateStory { title, priority, labels, assignee } => {
                crate::lock::with_project_lock(&self.root, || {
                    let snapshot = crate::storage::create_story(&self.root, &title)?;
                    let mut events_to_add = Vec::new();
                    if let Some(p) = priority {
                        events_to_add.push(crate::domain::StoryEvent::StoryPrioritySet {
                            at: crate::storage::now(),
                            priority: p,
                        });
                    }
                    if !labels.is_empty() {
                        events_to_add.push(crate::domain::StoryEvent::StoryLabelsSet {
                            at: crate::storage::now(),
                            labels,
                        });
                    }
                    if let Some(a) = assignee {
                        events_to_add.push(crate::domain::StoryEvent::StoryAssigned {
                            at: crate::storage::now(),
                            member_id: a,
                        });
                    }
                    if !events_to_add.is_empty() {
                        crate::storage::write_story_events(
                            &self.root, &snapshot.id, &events_to_add
                        )?;
                    }
                    Ok(())
                })?;
                self.dispatch(Action::RefreshData)?;
                self.dispatch(Action::Notify("Story created".into()))?;
            }
            Action::MoveStory { ref id, ref target_state } => {
                crate::lock::with_project_lock(&self.root, || {
                    crate::storage::write_story_events(&self.root, id, &[
                        crate::domain::StoryEvent::StoryStateChanged {
                            at: crate::storage::now(),
                            state: target_state.clone(),
                        }
                    ])?;
                    Ok(())
                })?;
                // Auto-archive if target is CLOSED
                if let Some(def) = self.state.data.state_map.get(target_state) {
                    if def.super_state == crate::domain::SuperState::Closed {
                        crate::storage::archive_story(&self.root, id)?;
                    }
                }
                self.dispatch(Action::RefreshData)?;
            }
            Action::SelectStory(id) => {
                self.state.selected_story_id = Some(id);
            }
            Action::SetFilter(spec) => {
                self.state.filters.push(spec);
            }
            Action::ClearFilter(index) => {
                if index < self.state.filters.len() {
                    self.state.filters.remove(index);
                }
            }
            Action::ClearAllFilters => {
                self.state.filters.clear();
            }
            Action::Notify(msg) => {
                self.status_bar.set_notification(msg);
            }
            // ... remaining mutation actions follow the same pattern:
            // lock -> write events -> refresh -> notify
            _ => {}
        }
        Ok(())
    }
}
```

### Public Entry Point

```rust
// src/tui/mod.rs

#[cfg(feature = "tui")]
mod action;
#[cfg(feature = "tui")]
mod app;
#[cfg(feature = "tui")]
mod components;
#[cfg(feature = "tui")]
mod data;
#[cfg(feature = "tui")]
mod event;
#[cfg(feature = "tui")]
mod focus;
#[cfg(feature = "tui")]
mod keymap;
#[cfg(feature = "tui")]
mod state;
#[cfg(feature = "tui")]
mod terminal;
#[cfg(feature = "tui")]
mod theme;

use std::path::Path;
use crate::error::AppError;

pub fn run(root: &Path) -> Result<(), AppError> {
    // Build a single-threaded tokio runtime for the TUI event loop.
    // We don't use #[tokio::main] because the caller (main.rs) is sync.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| AppError::Storage(format!("failed to start async runtime: {e}")))?;

    rt.block_on(async {
        let mut app = app::TuiApp::new(root)?;
        app.run().await
    })
}
```

## Cross-Cutting Concerns

### Error Handling

- Storage/IO errors during mutations are caught and displayed as status bar notifications rather than crashing the TUI. The pattern: `match self.dispatch(action) { Err(e) => self.status_bar.set_notification(format!("Error: {e}")), Ok(()) => {} }`.
- Terminal restore happens in a panic hook (ratatui 0.28.1+ does this automatically via `ratatui::init()`).
- File watcher errors are logged to a status bar warning on startup, then ignored -- manual refresh with `r` still works.

### Terminal State Safety

- `ratatui::init()` installs a panic hook that restores the terminal.
- The `run()` function has a `defer`-style pattern: `ratatui::restore()` is called on both normal exit and error paths.
- `$EDITOR` escape hatch: `ratatui::restore()` before spawning, `ratatui::init()` after it exits.

### Configuration

No new configuration files. The TUI reads:
- `.storyhook/states.toml` for column definitions (already exists).
- `.storyhook/project.toml` for prefix (already exists).
- `$NO_COLOR` environment variable for color disabling (standard).
- `$EDITOR` for external editor (standard).

Future: a `[tui]` section in `project.toml` for theme overrides. Not needed for v1.

### Performance

- Full data reload (all open stories) happens synchronously but is fast: reading ~100 JSONL files is < 10ms.
- File watcher debounce at 200ms prevents thrashing.
- No background threads for rendering -- the main loop is single-threaded async, which is simpler to reason about.
- Hit region tracking is rebuilt every render frame. At ~100 cards this is negligible.

## Key Design Decisions

1. **Sync storage reads, not async.** The existing `storage.rs` is entirely synchronous (fs, SQLite). Wrapping it in `tokio::task::spawn_blocking` adds complexity with no benefit -- reads are < 10ms. If projects grow to thousands of stories, we can add `spawn_blocking` later without changing any interfaces.

2. **No separate data cache layer.** `DataStore::load()` reads everything fresh each time. This avoids stale cache bugs and is simple. The file watcher triggers a full reload, not incremental updates. This is fine because storyhook projects typically have < 200 open stories.

3. **Board only shows OPEN states.** Closed/archived stories are not displayed on the board. The dashboard can show closed counts. This matches the mental model: the board is your active work.

4. **Components own local UI state, AppState owns domain data.** This is the hybrid TEA + Component pattern from the research. A component never reaches into another component's state. All cross-component communication goes through Actions dispatched to `TuiApp`.

5. **Feature flag rather than separate binary.** Keeps the project as a single crate. Users install the TUI with `cargo install storyhook --features tui`. The core `cargo install storyhook` stays lean for CI/agent environments.

---

**Relevant files referenced in this design:**

- `/home/mikey/storyhook/Cargo.toml` -- needs `[features]` section and optional TUI deps
- `/home/mikey/storyhook/src/main.rs` -- needs early intercept for `story tui`
- `/home/mikey/storyhook/src/lib.rs` -- needs `#[cfg(feature = "tui")] pub mod tui;`
- `/home/mikey/storyhook/src/domain.rs` -- reused types: `StorySnapshot`, `StoryEvent`, `StateDef`, `Priority`, `SuperState`, `Member`
- `/home/mikey/storyhook/src/storage.rs` -- reused functions: `load_states`, `load_all_open_snapshots`, `create_story`, `write_story_events`, `archive_story`, etc.
- `/home/mikey/storyhook/src/output.rs` -- reused data structures: `StoryView`, `SummaryView`
- `/home/mikey/storyhook/src/lock.rs` -- reused: `with_project_lock`
- `/home/mikey/storyhook/src/error.rs` -- reused: `AppError`
- `/home/mikey/storyhook/.planning/IDEA.md` -- requirements document
- `/home/mikey/storyhook/.planning/research/SUMMARY.md` -- research conclusions