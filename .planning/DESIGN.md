# Design: Storyhook TUI

## System Overview

The TUI is a `story tui` subcommand that launches an interactive terminal application built on ratatui + crossterm. It reuses the existing domain, storage, and output layers directly. All TUI dependencies are always-compiled (no feature flag).

```
main.rs  →  detects "tui" subcommand  →  tui::run()
                                            │
                    ┌───────────────────────┼──────────────────┐
                    │  tui/                 │                  │
                    │  event.rs ──→ app.rs ──→ terminal.rs    │
                    │  (crossterm +   (state +    (init/       │
                    │   file watch)    loop)       restore)    │
                    │                  │                       │
                    │    ┌─────────────┼──────────┐           │
                    │    │  components/ │          │           │
                    │    │  dashboard   │ data.rs  │ theme.rs  │
                    │    │  board       │ (bridge) │ (colors)  │
                    │    │  detail      │          │           │
                    │    │  filter_bar  │          │           │
                    │    │  create_form │          │           │
                    │    │  status_bar  │          │           │
                    │    │  help        │          │           │
                    │    └─────────────┘          │           │
                    └─────────────────────────────────────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    │  Existing storyhook crate      │
                    │  domain.rs  storage.rs         │
                    │  output.rs  lock.rs  error.rs  │
                    └───────────────────────────────┘
```

### Key Decisions
- **No tokio** — Blocking event loop with `crossterm::event::poll()`. Background threads for file watching and tick, communicating via `std::sync::mpsc`.
- **No feature flag** — Always compiled. Simpler code, no `#[cfg]` boilerplate.
- **Read without lock** — TUI reads data without holding the project lock. Lock acquired only for writes, then immediately released. Re-read after every write.
- **Hybrid TEA + Component** — Centralized `AppState` with `Action` enum at top level. Component trait for each view.

## Module Structure

```
src/
  main.rs                    # Modified: detect "tui" subcommand
  lib.rs                     # Modified: add `pub mod tui;`
  tui/
    mod.rs                   # pub fn run(root: &Path) -> Result<()>
    app.rs                   # TuiApp struct, main loop, action dispatch
    action.rs                # Action enum
    state.rs                 # AppState struct
    event.rs                 # EventSource: crossterm + file watcher + tick
    terminal.rs              # Terminal init/restore helpers
    theme.rs                 # ANSI color palette, Style constants
    data.rs                  # DataStore: bridge between storage.rs and TUI state
    keymap.rs                # Key → Action mapping tables
    focus.rs                 # FocusTarget enum, FocusStack
    components/
      mod.rs                 # Component trait definition
      dashboard.rs           # Dashboard view
      board.rs               # Board view (grouped table with state sections)
      story_detail.rs        # Story detail modal (~66% screen)
      filter_bar.rs          # Persistent filter bar
      create_form.rs         # Create story form modal
      status_bar.rs          # Bottom status bar
      help.rs                # Help overlay (keybinding reference)
```

### Dependencies to Add

```toml
[dependencies]
ratatui = { version = "0.30", default-features = false, features = ["crossterm"] }
crossterm = "0.28"
tui-textarea = "0.7"
tui-input = "0.11"
ratatui-macros = "0.6"
tui-scrollview = "0.6"
notify = "7"
```

### Existing File Changes (Minimal)

1. **`Cargo.toml`** — Add TUI dependencies
2. **`src/lib.rs`** — Add `pub mod tui;`
3. **`src/main.rs`** — Early intercept for `story tui` before CLI parsing

## Component Architecture

### Component Trait

```rust
pub trait Component {
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action>;
    fn handle_mouse(&mut self, mouse: MouseEvent, state: &AppState) -> Vec<Action>;
    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState);
    fn on_state_change(&mut self, state: &AppState) {}
    fn hit_regions(&self) -> &[HitRegion] { &[] }
}

pub struct HitRegion {
    pub rect: Rect,
    pub target: HitTarget,
}

pub enum HitTarget {
    StoryRow { id: String },
    SectionHeader { state_slug: String },
    FilterChip { index: usize },
    DashboardSection { name: String },
    Button { id: String },
}
```

### Dashboard

- **Purpose:** Home screen. Summary stats, state distribution, top-5 ready stories, recent activity.
- **Local state:** `selected_section: usize`, `scroll_offset: u16`
- **Keys:** `j/k` navigate, `Enter` drills into board or story, `n` opens create form
- **Actions:** `SwitchView(Board)`, `OpenDetail(id)`, `OpenCreateForm`

### Board (Grouped Table)

- **Purpose:** Primary view. Stories grouped by state as collapsible sections. One story per row.
- **Local state:**
  ```rust
  pub struct Board {
      cursor: usize,               // index into flat list of visible rows
      collapsed: HashSet<String>,   // collapsed state slugs
      hit_regions: Vec<HitRegion>,
  }
  ```
- **Keys:**
  - `j/k` or Up/Down — move cursor between rows (skipping collapsed sections)
  - `h/l` or `g/G` — jump to previous/next section header
  - `Space` — toggle section collapsed/expanded (when on section header)
  - `Enter` — open story detail modal (when on story row)
  - `>/<` or `H/L` — move selected story to next/previous state
  - `n` — open create form
  - `/` — focus filter bar
- **Actions:** `OpenDetail(id)`, `MoveStory { id, target_state }`, `OpenCreateForm`, `FocusFilterBar`

**Layout:**
```
▼ todo (5) ─────────────────────────────────────────────────────
  SH-12  Fix login flow bug     !! high  [bug]       @mikey  BLK
> SH-14  Refactor DB layer      .  low
  SH-15  Add search             !  med   [feature]
▼ in-progress (3) ──────────────────────────────────────────────
  SH-8   Add search endpoint    !  med               @mikey
  SH-11  Build TUI board        !!! crit [tui]       @mikey
▶ review (1) ───────────────────────────────────────────────────
▼ done (2) ─────────────────────────────────────────────────────
  SH-3   Init project
  SH-7   Setup CI                          [infra]
```

- `>` cursor marker on selected row
- `▼`/`▶` disclosure triangles for expanded/collapsed
- Section header shows state name + count (count is always visible even when collapsed)
- Story rows: ID (dimmed), title (bold, fills available space), priority symbol, labels in brackets, assignee with `@`, `BLK` badge if awaiting

### StoryDetail (Modal)

- **Purpose:** ~66% screen modal overlay for viewing and editing a story.
- **Local state:**
  ```rust
  pub struct StoryDetail {
      mode: DetailMode,        // Viewing, EditingTitle, EditingPriority, etc.
      selected_field: usize,   // which field is highlighted
      title_input: tui_input::Input,
      comment_textarea: tui_textarea::TextArea<'static>,
      priority_cursor: usize,
      label_input: tui_input::Input,
      assignee_input: tui_input::Input,
      scroll_offset: u16,
  }
  ```
- **Modes:** Viewing → press `e` → editing the selected field. `Enter` confirms, `Esc` cancels.
- **Keys (Viewing):** `j/k` select field, `e` edit, `c` add comment, `Ctrl+E` open `$EDITOR`, `>/<` change state, `Esc` close
- **Keys (Editing):** Text input in the active field, `Enter` confirm, `Esc` cancel, `Tab`/`Shift+Tab` cycle fields
- **Actions:** `UpdateTitle`, `SetPriority`, `SetLabels`, `AssignStory`, `AddComment`, `MoveStory`, `SetAwaiting`, `ClearAwaiting`, `CloseModal`

### FilterBar

- **Purpose:** Persistent bar above the board. Active filter chips + type-to-filter input.
- **Local state:** `input: tui_input::Input`, `focused: bool`, `suggestions: Vec<String>`, `suggestion_cursor: usize`
- **Keys (focused):** Text input, `Enter` apply filter, `Tab` accept suggestion, `Backspace` (empty) remove last chip, `Esc` unfocus, `Ctrl+U` clear all
- **Actions:** `SetFilter(FilterSpec)`, `ClearFilter(index)`, `ClearAllFilters`, `UnfocusFilterBar`

### CreateForm (Modal)

- **Purpose:** Modal form for new story. Fields: title, priority, labels, assignee.
- **Local state:** Field inputs, `focused_field: usize`
- **Keys:** `Tab`/`Shift+Tab` cycle fields, `Enter` submit (from any field if title non-empty), `Esc` cancel
- **Actions:** `CreateStory { title, priority, labels, assignee }`, `CloseModal`

### StatusBar

- **Purpose:** Bottom bar. Context-sensitive key hints (left), notification (center, 3s timeout), view label + story count (right).
- **Local state:** `notification: Option<(String, Instant)>`
- Passive — handles no events, emits no actions.

### Help (Overlay)

- **Purpose:** Full keybinding reference.
- **Local state:** `scroll_offset: u16`
- **Keys:** `j/k` scroll, `Esc`/`q`/`?` close
- **Actions:** `CloseModal`

## Data Flow

### DataStore Bridge

```rust
pub struct DataStore {
    pub states: Vec<StateDef>,
    pub state_map: BTreeMap<String, StateDef>,
    pub stories: Vec<StorySnapshot>,
    pub prefix: String,
    pub members: Vec<Member>,
}

impl DataStore {
    /// Load everything from disk WITHOUT holding the project lock.
    /// Called on startup and on every refresh.
    pub fn load(root: &Path) -> Result<Self, AppError> { ... }

    /// Stories grouped by state slug, in state definition order.
    /// Only OPEN states shown on board.
    pub fn stories_by_state(&self) -> Vec<(&StateDef, Vec<&StorySnapshot>)> { ... }

    pub fn find_story(&self, id: &str) -> Option<&StorySnapshot> { ... }
}
```

### Read/Write Pattern

- **Reads:** `DataStore::load()` reads JSONL files and states.toml without locking. Note: `write_story_events` uses append mode (not atomic write-then-rename), so a concurrent read could encounter a partial trailing JSON line. The read path MUST gracefully skip incomplete trailing lines rather than propagating a parse error.
- **Writes:** Acquire lock via `lock::with_project_lock()`, perform ALL mutations including `archive_story` inside the lock closure, release lock, then `DataStore::load()` to refresh. This matches the existing CLI pattern in `app.rs`.
- **Stale modal protection:** After every `RefreshData`, if a `StoryDetail` modal is open, check whether the story still exists in the refreshed dataset. If not (e.g., archived externally), close the modal and show a notification.
- **Refresh triggers:** File watcher (`notify` watching `.storyhook/open/stories/`), manual `r` key, automatic after every write.

### Event System

```rust
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    DataChanged,    // from file watcher (debounced 200ms)
    Tick,           // 250ms for notification expiry
}
```

Three background threads, all sending via `std::sync::mpsc::Sender<Event>`:
1. **Input thread** — `crossterm::event::poll(100ms)` + `event::read()`
2. **File watcher thread** — `notify::recommended_watcher` on `.storyhook/open/stories/`, debounced 200ms
3. **Tick thread** — `thread::sleep(250ms)` loop

Main loop: `receiver.recv()` → route event → dispatch actions → render.

## Focus Management

```rust
pub enum FocusTarget {
    Dashboard,
    Board,
}

pub enum Modal {
    StoryDetail { story_id: String },
    CreateForm,
    Help,
}

pub struct FocusStack {
    pub base: FocusTarget,
    pub modals: Vec<Modal>,
}
```

- When `modals` is non-empty, the top modal gets all keyboard input (focus trapping).
- Mouse clicks outside a modal close it.
- `FocusStack::push_modal()` / `pop_modal()` for open/close.

## Keyboard Shortcuts

### Global (no modal open)

| Key | Action |
|-----|--------|
| `q` | Quit (unless text input focused) |
| `Ctrl+C` | Force quit |
| `?` | Toggle help overlay |
| `1` | Switch to Dashboard |
| `2` | Switch to Board |
| `r` | Refresh data from disk |

### Board View

| Key | Action |
|-----|--------|
| `j` / Down | Next row |
| `k` / Up | Previous row |
| `h` | Jump to previous section header |
| `l` | Jump to next section header |
| `g` | Jump to first row |
| `G` | Jump to last row |
| `Space` | Toggle section collapsed/expanded (on header) |
| `Enter` | Open story detail (on story row) |
| `>` / `L` | Move story to next state |
| `<` / `H` | Move story to previous state |
| `n` | Open create form |
| `/` | Focus filter bar |

### Filter Bar (focused)

| Key | Action |
|-----|--------|
| Text input | Type filter query |
| `Enter` | Apply filter |
| `Tab` | Accept suggestion |
| `Backspace` (empty) | Remove last chip |
| `Esc` | Unfocus, return to board |
| `Ctrl+U` | Clear all filters |

### Story Detail Modal

| Key | Action |
|-----|--------|
| `Esc` | Close modal (or cancel edit) |
| `j` / Down | Next field |
| `k` / Up | Previous field |
| `e` | Edit selected field |
| `Enter` | Confirm edit |
| `c` | Add comment |
| `Ctrl+E` | Open `$EDITOR` |
| `>` | Move to next state |
| `<` | Move to previous state |
| `Tab` / `Shift+Tab` | Cycle fields (during edit) |

### Create Form Modal

| Key | Action |
|-----|--------|
| `Esc` | Cancel and close |
| `Tab` / `Shift+Tab` | Cycle fields |
| `Enter` | Submit (if title non-empty) |

### Help Overlay

| Key | Action |
|-----|--------|
| `Esc` / `q` / `?` | Close |
| `j` / `k` | Scroll |

## Mouse Interaction

### Phase 1 (Initial): Click and Scroll

| Interaction | Behavior |
|-------------|----------|
| Click on story row | Select that row |
| Double-click on story row | Open detail modal |
| Click on section header | Select header |
| Click on filter chip | Remove that filter |
| Click on filter bar | Focus filter bar |
| Scroll wheel | Scroll the board vertically |
| Click outside modal | Close modal |

### Phase 2 (Deferred): Drag-and-Drop

Drag a story row to a different section header to change its state. State machine:
```
Idle → MouseDown on story row → DragPending
DragPending → MouseDrag (>3 cells) → Dragging
Dragging → MouseUp on section header → Drop (move story)
Dragging → Esc → Cancel
DragPending → MouseUp (no drag) → Click (select)
```

Visual feedback: dimmed source row, highlighted target section header.

## Theme

ANSI 16 named colors. Honors `$NO_COLOR` environment variable.

| Element | Style |
|---------|-------|
| Story ID | DarkGray |
| Story title | White bold |
| Priority critical `!!!` | Red bold |
| Priority high `!!` | Yellow |
| Priority medium `!` | Blue |
| Priority low `.` | DarkGray |
| Labels `[tag]` | Cyan |
| Assignee `@name` | Green |
| Blocked badge `BLK` | Red bold |
| Section header | White bold |
| Section count | DarkGray |
| Cursor `>` | Cyan bold |
| Selected row | Cyan background (subtle) |
| Filter chip | Black on Cyan |
| Status bar | DarkGray |
| Status bar keys | Yellow bold |
| Modal border | Cyan |
| Disclosure `▼`/`▶` | DarkGray |

`$NO_COLOR` mode: all colors removed, uses Bold/Dim/Underline modifiers only.

## Action Enum

```rust
pub enum Action {
    // Navigation
    SwitchView(View),
    OpenDetail(String),
    OpenCreateForm,
    CloseModal,
    FocusFilterBar,
    UnfocusFilterBar,
    ToggleHelp,

    // Board
    ToggleSection(String),  // state slug

    // Data mutations
    CreateStory { title: String, priority: Option<Priority>, labels: Vec<String>, assignee: Option<String> },
    MoveStory { id: String, target_state: String },
    UpdateTitle { id: String, title: String },
    SetPriority { id: String, priority: Priority },
    SetLabels { id: String, labels: Vec<String> },
    AssignStory { id: String, assignee: String },
    AddComment { id: String, text: String },
    SetAwaiting { id: String, reason: String },
    ClearAwaiting { id: String },

    // Filtering
    SetFilter(FilterSpec),
    ClearFilter(usize),
    ClearAllFilters,

    // System
    RefreshData,
    Notify(String),
    Quit,
}

pub enum View { Dashboard, Board }
```

## Integration Points

### Reused from Existing Codebase (No Modifications)

| Module | What TUI uses |
|--------|---------------|
| `domain.rs` | `StorySnapshot`, `StoryEvent`, `StateDef`, `Priority`, `SuperState`, `Member`, `StoryRelation`, `StoryComment`, `fold_story` |
| `storage.rs` | `load_states`, `load_state_map`, `load_all_open_snapshots`, `load_members`, `load_project_prefix`, `create_story`, `write_story_events`, `ensure_project`, `now` |
| `output.rs` | `StoryView`, `SummaryView` (data structures, not rendering) |
| `lock.rs` | `with_project_lock` (for all mutations) |
| `error.rs` | `AppError` |

### New TUI Code

| File | Responsibility |
|------|---------------|
| `data.rs` | Thin bridge: calls `storage::load_*`, assembles `DataStore` |
| `event.rs` | Event source: 3 background threads → mpsc → main loop |
| `app.rs` | Main loop, action dispatch, orchestrates components |
| `components/*` | All rendering + event handling for each view |
| `theme.rs` | Style definitions, `$NO_COLOR` support |
| `focus.rs` | Focus target + modal stack |
| `keymap.rs` | Key → Action mapping tables |
| `terminal.rs` | `ratatui::init()` / `ratatui::restore()` |

### Write Path

All mutations:
1. Acquire lock via `lock::with_project_lock()`
2. Call existing `storage::*` functions
3. Release lock
4. `DataStore::load()` to refresh
5. Emit `Action::Notify("Story SH-X updated")` for status bar feedback
