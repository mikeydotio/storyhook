# Implementation Plan: Storyhook TUI

**Created:** 2026-03-28
**Source:** IDEA.md, DESIGN.md, Research SUMMARY.md
**Status:** NOT STARTED

## Architecture Summary

The TUI is a `story tui` subcommand that launches an interactive terminal application built on ratatui + crossterm. It reuses the existing domain, storage, lock, and error modules directly. No tokio. No feature flags. Blocking event loop with `std::sync::mpsc`. Board is a grouped table (not vertical Kanban columns). ANSI 16 colors with `$NO_COLOR` support. Read without lock; write with lock.

## Existing Codebase Facts

- **Crate:** `storyhook` v0.6.0, Rust 2024 edition, rust-version 1.89
- **Binary:** `story` at `src/main.rs`
- **Modules:** domain, storage, lock, error, cli, app, output, hooks, mcp, plugin, decompose, help_topics
- **No TUI code exists yet** -- `src/tui/` directory absent, no "tui" in CLI parser
- **`main.rs` pattern:** Early intercepts (e.g., `--mcp`) happen before `cli::parse_invocation()`
- **Lock pattern:** `lock::with_project_lock(root, || { ... })` -- closure-based, 5s timeout
- **Storage reads:** `load_states`, `load_state_map`, `load_all_open_snapshots`, `load_members`, `load_project_prefix`
- **Storage writes:** `create_story`, `write_story_events`, `rewrite_story_events`
- **Domain types:** `StorySnapshot`, `StoryEvent`, `StateDef`, `Priority`, `SuperState`, `Member`, `StoryComment`, `StoryRelation`, `fold_story`

---

## Wave 0 -- Foundation (Skeleton + Dependencies)

**Goal:** Project compiles with all TUI dependencies. The `story tui` subcommand launches, shows a blank terminal, and exits cleanly on `q` or `Ctrl+C`. This validates the dependency chain, terminal init/restore, and subcommand integration.

**Resumption state after Wave 0:** A minimal TUI loop runs. No views rendered. Terminal properly restored on exit. All crate dependencies resolve.

### Task 0.1: Add TUI Dependencies to Cargo.toml
- **Description:** Add ratatui, crossterm, tui-textarea, tui-input, ratatui-macros, tui-scrollview, and notify to `[dependencies]` in Cargo.toml per DESIGN.md versions.
- **Acceptance:** `cargo check` passes with all new dependencies.
- **Files:** `/home/mikey/storyhook/Cargo.toml`
- **Complexity:** S

### Task 0.2: Create TUI Module Skeleton
- **Description:** Create `src/tui/mod.rs` with a `pub fn run(root: &Path) -> Result<(), AppError>` stub that returns `Ok(())`. Create placeholder files for all submodules listed in DESIGN.md (app.rs, action.rs, state.rs, event.rs, terminal.rs, theme.rs, data.rs, keymap.rs, focus.rs, components/mod.rs, components/dashboard.rs, components/board.rs, components/story_detail.rs, components/filter_bar.rs, components/create_form.rs, components/status_bar.rs, components/help.rs). Add `pub mod tui;` to `src/lib.rs`.
- **Acceptance:** `cargo check` passes. `src/tui/mod.rs` exports `run()`. All submodule files exist and are referenced from `mod.rs`.
- **Files:** `/home/mikey/storyhook/src/lib.rs`, `/home/mikey/storyhook/src/tui/mod.rs`, all submodule files listed above
- **Complexity:** S

### Task 0.3: Wire `story tui` Subcommand in main.rs
- **Description:** Add early intercept in `main.rs` -- before CLI parsing, check if `raw_args` starts with `"tui"`. If so, resolve `cwd`, call `storyhook::tui::run(&cwd)`, handle errors, and exit. This mirrors the `--mcp` pattern.
- **Acceptance:** Running `story tui` in a storyhook project directory calls `tui::run()` and exits with code 0. Running `story tui` outside a project directory produces an appropriate error.
- **Files:** `/home/mikey/storyhook/src/main.rs`
- **Complexity:** S

### Task 0.4: Terminal Init/Restore
- **Description:** Implement `src/tui/terminal.rs` with functions to initialize (enter alternate screen, enable raw mode, enable mouse capture, create ratatui Terminal) and restore (leave alternate screen, disable raw mode, disable mouse capture) the terminal. Include a panic hook that restores terminal state.
- **Acceptance:** Running `story tui` enters alternate screen. Pressing `q` exits and restores terminal to normal state. A `panic!()` injected for testing also restores terminal state.
- **Files:** `/home/mikey/storyhook/src/tui/terminal.rs`
- **Complexity:** S

### Task 0.5: Minimal Event Loop
- **Description:** Implement a minimal blocking event loop in `src/tui/app.rs`. Use `crossterm::event::poll(Duration::from_millis(100))` + `crossterm::event::read()` directly (not yet the full mpsc architecture). Handle `KeyCode::Char('q')` and `Ctrl+C` to quit. Render a single centered "Storyhook TUI" text on each frame. Wire this into `tui::run()`.
- **Acceptance:** `story tui` shows centered text. `q` and `Ctrl+C` both exit cleanly. Terminal is restored after exit.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`, `/home/mikey/storyhook/src/tui/mod.rs`
- **Complexity:** S

### Task 0.6: Wave 0 Verification
- **Description:** Manual smoke test: `cargo build`, `story tui` launches, shows text, `q` exits, `Ctrl+C` exits, terminal is clean after exit. Verify `cargo clippy` and `cargo test` pass (existing tests unbroken).
- **Acceptance:** All checks pass. No regressions in existing test suite.
- **Files:** None (verification only)
- **Complexity:** S

---

## Wave 1 -- Core Infrastructure (Theme, Actions, State, Events, Data Bridge, Focus)

**Goal:** The infrastructure layer is complete. The Action enum, AppState, FocusStack, theme, keymap, event system (3 threads + mpsc), and DataStore bridge are all implemented and tested. The main loop uses the full mpsc event architecture. No views yet, but the plumbing is ready.

**Resumption state after Wave 1:** Full event system running (crossterm input, file watcher, tick timer). DataStore loads real project data. Actions can be dispatched. Focus stack manages view/modal state. Theme constants defined.

**Depends on:** Wave 0 complete.

### Task 1.1: Action Enum + View Enum
- **Description:** Implement `src/tui/action.rs` with the full `Action` enum and `View` enum as specified in DESIGN.md. Include derives for `Debug`, `Clone`. The `FilterSpec` type can be a simple struct with optional fields for now (text query, state, assignee, priority, label, blocked/ready/stale).
- **Acceptance:** All action variants from DESIGN.md are present. `cargo check` passes.
- **Files:** `/home/mikey/storyhook/src/tui/action.rs`
- **Complexity:** S

### Task 1.2: AppState Struct
- **Description:** Implement `src/tui/state.rs` with `AppState` containing: `data: DataStore`, `focus: FocusStack`, `view: View`, `filters: Vec<FilterSpec>`, `running: bool`, `notification: Option<(String, Instant)>`, `terminal_size: (u16, u16)`. Include constructor that takes a `DataStore`.
- **Acceptance:** `AppState::new(data)` creates a valid initial state with Dashboard view, empty focus stack, no filters, running=true.
- **Files:** `/home/mikey/storyhook/src/tui/state.rs`
- **Complexity:** S

### Task 1.3: Theme Module
- **Description:** Implement `src/tui/theme.rs` with all style constants from DESIGN.md's theme table. Check `$NO_COLOR` env var at startup -- if set, all styles use only Bold/Dim/Underline modifiers with no color. Provide a `Theme` struct or module-level functions/constants that return `ratatui::style::Style` values.
- **Acceptance:** Each element in the DESIGN.md theme table has a corresponding style constant. When `NO_COLOR=1` is set, all styles have `Color::Reset` (no color). Unit tests verify at least 3 style values in both modes.
- **Files:** `/home/mikey/storyhook/src/tui/theme.rs`
- **Complexity:** S

### Task 1.4: Focus Management
- **Description:** Implement `src/tui/focus.rs` with `FocusTarget`, `Modal`, and `FocusStack` as specified in DESIGN.md. Methods: `push_modal()`, `pop_modal()`, `top_modal() -> Option<&Modal>`, `has_modal() -> bool`, `base() -> &FocusTarget`.
- **Acceptance:** Unit tests verify: push/pop modal lifecycle, `top_modal()` returns the most recently pushed modal, `has_modal()` correctness, popping from empty modals is safe.
- **Files:** `/home/mikey/storyhook/src/tui/focus.rs`
- **Complexity:** S

### Task 1.5: DataStore Bridge
- **Description:** Implement `src/tui/data.rs` with the `DataStore` struct as specified in DESIGN.md. `DataStore::load(root)` calls `storage::ensure_project()`, `storage::load_states()`, `storage::load_state_map()`, `storage::load_all_open_snapshots()`, `storage::load_members()`, `storage::load_project_prefix()`. Implement `stories_by_state()` (grouped by state slug, in state definition order, only OPEN superstates), `find_story()`, and a `story_count()` helper.
- **Acceptance:** Unit test using a tempdir with `storage::init_project()` + a few created stories: `DataStore::load()` returns correct state count, story count, `stories_by_state()` groups correctly, `find_story()` finds by ID.
- **Files:** `/home/mikey/storyhook/src/tui/data.rs`
- **Complexity:** M

### Task 1.6: Event System (3 Threads + mpsc)
- **Description:** Implement `src/tui/event.rs` with the `Event` enum (`Key`, `Mouse`, `Resize`, `DataChanged`, `Tick`) and an `EventSource` struct that spawns 3 background threads (input, file watcher, tick) all sending to a shared `mpsc::Sender<Event>`. The input thread uses `crossterm::event::poll(100ms)` + `read()`. The file watcher uses `notify::recommended_watcher` watching `.storyhook/open/stories/` with 200ms debounce. The tick thread sends `Tick` every 250ms. Provide `EventSource::new(root) -> (EventSource, mpsc::Receiver<Event>)` and `EventSource::stop()` (or drop-based cleanup).
- **Acceptance:** Integration test: create EventSource, verify Tick events arrive, verify a file change in the watched directory triggers DataChanged (within 500ms), verify keyboard simulation works (or at minimum that the input thread starts without error).
- **Files:** `/home/mikey/storyhook/src/tui/event.rs`
- **Complexity:** M

### Task 1.7: Keymap Module
- **Description:** Implement `src/tui/keymap.rs` with functions that map `(KeyEvent, context) -> Option<Action>` for each context: global, board, filter_bar, story_detail, create_form, help. Contexts correspond to focus states. All keybindings from DESIGN.md's keyboard shortcuts tables must be covered.
- **Acceptance:** Unit tests verify: `q` in global context -> `Action::Quit`, `j` in board context -> cursor movement (board handles internally, so keymap returns None and board handles it), `?` in global -> `Action::ToggleHelp`, `/` in board -> `Action::FocusFilterBar`, `Esc` in story detail -> `Action::CloseModal`. At least 10 key mappings tested.
- **Files:** `/home/mikey/storyhook/src/tui/keymap.rs`
- **Complexity:** M

### Task 1.8: Upgrade Main Loop to mpsc Architecture
- **Description:** Refactor `src/tui/app.rs` to use the full mpsc event system from Task 1.6. The main loop becomes: `receiver.recv()` -> route event -> dispatch actions -> render. Integrate `AppState`, `FocusStack`, `Action` dispatch. For now, actions just modify state (quit works, view switching works) but no component rendering yet -- just render the current view name and story count from DataStore.
- **Acceptance:** `story tui` launches with real data loaded from DataStore, shows view name + story count. `q` quits. `1`/`2` switches between Dashboard/Board labels. File changes in `.storyhook/` trigger a data reload (verify by watching story count update after CLI creates a story in another terminal).
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`, `/home/mikey/storyhook/src/tui/mod.rs`
- **Complexity:** M

### Task 1.9: Component Trait Definition
- **Description:** Define the `Component` trait in `src/tui/components/mod.rs` as specified in DESIGN.md: `handle_key()`, `handle_mouse()`, `render()`, `on_state_change()`, `hit_regions()`. Also define `HitRegion` and `HitTarget` structs/enums.
- **Acceptance:** Trait compiles. A trivial test component can implement it.
- **Files:** `/home/mikey/storyhook/src/tui/components/mod.rs`
- **Complexity:** S

### Task 1.10: Wave 1 Verification
- **Description:** Run `cargo test`, `cargo clippy`, verify all unit tests from this wave pass. Manual smoke test: `story tui` launches, loads real data, shows story count, `q` exits, `1`/`2` switches views, file watcher triggers refresh.
- **Acceptance:** All tests pass. No clippy warnings in tui modules. Manual smoke test succeeds.
- **Files:** None (verification only)
- **Complexity:** S

---

## Wave 2 -- Board View + Status Bar (Primary View)

**Goal:** The board view renders stories grouped by state as a scrollable grouped table. Section headers show disclosure triangles and counts. Story rows show ID, title, priority, labels, assignee, blocked indicator. Cursor navigation works. Sections are collapsible. The status bar shows contextual key hints. This is the most important view and must be solid before building others.

**Resumption state after Wave 2:** Board view is fully navigable with keyboard. Status bar shows hints. No modals yet. No filter bar yet. No mouse yet.

**Depends on:** Wave 1 complete.

### Task 2.1: Board Component -- Data Model + Flat Row Index
- **Description:** Implement the `Board` struct in `src/tui/components/board.rs` with its local state: `cursor: usize`, `collapsed: HashSet<String>`, `hit_regions: Vec<HitRegion>`. Implement a method `build_visible_rows(state: &AppState) -> Vec<RowItem>` where `RowItem` is an enum of `SectionHeader { slug, count }` or `StoryRow { snapshot ref/id }`. The flat list respects collapsed sections (collapsed sections show only the header, no story rows). Implement cursor clamping.
- **Acceptance:** Unit test: given a DataStore with 3 states and 5 stories, `build_visible_rows()` returns correct sequence. Collapsing a state removes its story rows but keeps the header. Cursor clamps to valid range.
- **Files:** `/home/mikey/storyhook/src/tui/components/board.rs`
- **Complexity:** M

### Task 2.2: Board Component -- Rendering
- **Description:** Implement `Component::render()` for Board. Render the grouped table layout from DESIGN.md: `>` cursor marker, `V`/`>` disclosure triangles (using Unicode), section headers with state name + count, story rows with ID (dimmed), title (bold), priority symbol (`!!!`/`!!`/`!`/`.`), labels in brackets, assignee with `@`, `BLK` badge. Use theme styles. Handle scrolling when rows exceed terminal height (the cursor row should always be visible). Populate `hit_regions` during render for future mouse support.
- **Acceptance:** Manual verification: board renders with proper alignment and styling. Stories are grouped correctly. Priority symbols, labels, assignee, BLK badge all display. Scrolling works when >20 stories exist (cursor stays visible).
- **Files:** `/home/mikey/storyhook/src/tui/components/board.rs`
- **Complexity:** L

### Task 2.3: Board Component -- Keyboard Navigation
- **Description:** Implement `Component::handle_key()` for Board. Handle: `j`/Down (next row), `k`/Up (previous row), `h` (jump to previous section header), `l` (jump to next section header), `g` (first row), `G` (last row), `Space` (toggle section collapse when on header), `Enter` (emit `Action::OpenDetail(id)` when on story row), `>`/`L` (emit `Action::MoveStory` to next state), `<`/`H` (emit `Action::MoveStory` to previous state), `n` (emit `Action::OpenCreateForm`), `/` (emit `Action::FocusFilterBar`).
- **Acceptance:** Unit test: cursor movement cycles through rows correctly, skipping collapsed sections. `Space` on a header toggles collapse. `Enter` on a story row returns `OpenDetail` action. `>`/`<` on a story row return `MoveStory` with correct target state (next/previous in state definition order).
- **Files:** `/home/mikey/storyhook/src/tui/components/board.rs`
- **Complexity:** M

### Task 2.4: Status Bar Component
- **Description:** Implement `src/tui/components/status_bar.rs`. Renders a bottom bar with: context-sensitive key hints on the left (changes based on current view and focus), notification message in the center (with 3-second timeout driven by Tick events), view label + story count on the right. Passive component -- no event handling.
- **Acceptance:** Manual verification: status bar displays key hints for board view. Notification appears and auto-clears after 3 seconds. View label and count are correct.
- **Files:** `/home/mikey/storyhook/src/tui/components/status_bar.rs`
- **Complexity:** S

### Task 2.5: Wire Board + Status Bar into Main Loop
- **Description:** Update `app.rs` to create Board and StatusBar components. When `view == View::Board`, render the board in the main area and status bar at the bottom. Route key events to the board component. Dispatch returned actions (MoveStory performs the write via lock, then refreshes data). Wire `Action::Notify` to update notification in AppState. Handle `Action::RefreshData` (manual `r` key).
- **Acceptance:** `story tui` shows the board view with real stories. Keyboard navigation works. `>` moves a story to the next state (verified by checking the JSONL file or running `story list`). `r` refreshes data. Status bar shows key hints and notifications ("Story moved to [state]" appears briefly after a move).
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`
- **Complexity:** M

### Task 2.6: Write Path -- Move Story
- **Description:** Implement the write path for `Action::MoveStory { id, target_state }` in the action dispatch layer. Acquires lock via `lock::with_project_lock()`, calls `storage::write_story_events()` with a `StoryStateChanged` event, releases lock, calls `DataStore::load()` to refresh, emits `Notify`. Handle errors gracefully (show error in notification, don't crash).
- **Acceptance:** Moving a story via `>` key updates the JSONL file. The board refreshes to show the story in its new state section. Lock errors (e.g., another process holding the lock) show an error notification instead of crashing.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs` (or a new `src/tui/commands.rs` if cleaner)
- **Complexity:** M

### Task 2.7: Wave 2 Verification
- **Description:** Full manual test: navigate board, collapse/expand sections, move stories between states, verify data persistence, verify status bar updates. Run `cargo test`, `cargo clippy`.
- **Acceptance:** All tests pass. Board is fully navigable. Stories can be moved. Status bar is functional. No clippy warnings.
- **Files:** None (verification only)
- **Complexity:** S

---

## Wave 3 -- Story Detail Modal + Create Form

**Goal:** The story detail modal opens on `Enter`, showing all story fields with inline editing. The create form modal opens on `n`, allowing new story creation. Both modals trap focus and close on `Esc` or click outside.

**Resumption state after Wave 3:** All CRUD operations work. Stories can be created, viewed, and edited (title, priority, labels, assignee, comments, state, awaiting). Modals render correctly as overlays.

**Depends on:** Wave 2 complete.

### Task 3.1: Modal Rendering Infrastructure
- **Description:** Implement a `render_modal()` helper function (in `components/mod.rs` or a shared utils module) that takes a `Frame`, computes a centered ~66% area, draws a `Block` with border (Cyan per theme), clears the interior, and returns the inner `Rect` for the modal content. This is shared by StoryDetail, CreateForm, and Help.
- **Acceptance:** Unit test: given various terminal sizes, the computed modal area is approximately 66% of the screen and centered. Border renders with correct style.
- **Files:** `/home/mikey/storyhook/src/tui/components/mod.rs` (or a new `src/tui/components/modal.rs`)
- **Complexity:** S

### Task 3.2: Story Detail Component -- Viewing Mode
- **Description:** Implement `src/tui/components/story_detail.rs`. In viewing mode, display all story fields: title, state, priority, assignee, labels, awaiting status, relationships, comments (scrollable), timestamps. Fields are listed vertically with labels on the left and values on the right. `j`/`k` or Up/Down selects the field. The selected field is highlighted. `Esc` closes the modal. `>`/`<` changes story state.
- **Acceptance:** Manual verification: detail modal shows all fields for a story. Navigation between fields works. Esc closes. State change via `>`/`<` works (reuses MoveStory write path).
- **Files:** `/home/mikey/storyhook/src/tui/components/story_detail.rs`
- **Complexity:** M

### Task 3.3: Story Detail Component -- Editing Mode
- **Description:** Implement inline editing in story detail. Pressing `e` on the selected field enters editing mode for that field. Supported editable fields: title (tui-input), priority (cycle through options with cursor), labels (tui-input, comma-separated), assignee (tui-input). `Enter` confirms the edit (writes via lock + storage), `Esc` cancels. `c` opens a comment input area (tui-textarea). `Tab`/`Shift+Tab` cycles between fields during edit.
- **Acceptance:** Each editable field can be modified. Title edit updates the story title (verified via `story show`). Priority edit changes priority. Label edit changes labels. Assignee edit changes assignee. Comment add appends a comment. All writes use the lock correctly. Canceling an edit (Esc) reverts to original value.
- **Files:** `/home/mikey/storyhook/src/tui/components/story_detail.rs`
- **Complexity:** L

### Task 3.4: Write Paths -- All Story Mutations
- **Description:** Implement write paths for all data mutation actions: `UpdateTitle`, `SetPriority`, `SetLabels`, `AssignStory`, `AddComment`, `SetAwaiting`, `ClearAwaiting`, `CreateStory`. Each follows the pattern: acquire lock, build appropriate `StoryEvent` variant, call `storage::write_story_events()`, release lock, refresh DataStore, emit Notify. For `CreateStory`, call `storage::create_story()` then optionally write additional events for priority/labels/assignee.
- **Acceptance:** Each mutation action correctly persists data. Integration test: create story via TUI, verify it appears in `story list`. Edit title via TUI, verify via `story show`. Add comment, verify. Change priority, verify.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs` (or `src/tui/commands.rs`)
- **Complexity:** M

### Task 3.5: Create Form Component
- **Description:** Implement `src/tui/components/create_form.rs`. A modal form with fields: title (tui-input, required), priority (cycle selector), labels (tui-input, comma-separated), assignee (tui-input). `Tab`/`Shift+Tab` cycles fields. `Enter` submits if title is non-empty (emits `CreateStory` action). `Esc` cancels and closes.
- **Acceptance:** Manual test: `n` opens create form. Tab cycles fields. Entering a title and pressing Enter creates a story. The board updates to show the new story. Esc cancels without creating.
- **Files:** `/home/mikey/storyhook/src/tui/components/create_form.rs`
- **Complexity:** M

### Task 3.6: Wire Modals into Focus Stack + Main Loop
- **Description:** Update `app.rs` to handle modal lifecycle. `Action::OpenDetail(id)` pushes `Modal::StoryDetail { story_id }` onto focus stack, creates StoryDetail component. `Action::OpenCreateForm` pushes `Modal::CreateForm`. `Action::CloseModal` pops the top modal. When a modal is open, all key events route to the top modal's component. Render modals as overlays on top of the current view.
- **Acceptance:** `Enter` on a board row opens story detail modal. `Esc` closes it, returning to board. `n` opens create form. Focus is trapped in modals. Multiple modal opens/closes work without state corruption.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`
- **Complexity:** M

### Task 3.7: Wave 3 Verification
- **Description:** Full CRUD test: create a story via create form, view it via detail modal, edit its title/priority/labels/assignee, add a comment, move it to another state, close modal, verify board reflects all changes. Run `cargo test`, `cargo clippy`.
- **Acceptance:** All CRUD operations work end-to-end. Data persists correctly. Modals render properly. All tests pass. No clippy warnings.
- **Files:** None (verification only)
- **Complexity:** S

---

## Wave 4 -- Filter Bar + Dashboard

**Goal:** The filter bar enables searching and filtering stories on the board. The dashboard provides a project summary home screen. Both views are fully navigable.

**Resumption state after Wave 4:** All views functional. Filter bar filters stories. Dashboard shows project metrics. View switching between Dashboard and Board works.

**Depends on:** Wave 3 complete.

### Task 4.1: Filter Bar Component
- **Description:** Implement `src/tui/components/filter_bar.rs`. Renders above the board area. Shows active filter chips (styled per theme) and a text input (tui-input) on the right. When focused: text input is active, `Enter` applies filter (parses input as `field:value` or free text search), `Tab` accepts suggestion, `Backspace` on empty input removes last chip, `Esc` unfocuses, `Ctrl+U` clears all filters. When unfocused: displays chips + hint text, clicking focuses it.
- **Acceptance:** Unit test: filter parsing correctly identifies `state:todo`, `priority:high`, `assignee:mikey`, `label:bug`, `blocked`, `ready`, free text. Chip add/remove works. Clearing works.
- **Files:** `/home/mikey/storyhook/src/tui/components/filter_bar.rs`
- **Complexity:** M

### Task 4.2: Filter Application Logic
- **Description:** Implement filtering in DataStore or AppState. When filters are active, `stories_by_state()` returns only stories matching all active filters. Filter types: text (substring match on title/ID), state (exact match on state slug), assignee (exact match), priority (match level), label (any label matches), blocked (has awaiting), ready (no awaiting and not blocked by relationships), stale (updated_at older than threshold).
- **Acceptance:** Unit test: create stories with various attributes, apply filters, verify correct subset returned. Multiple simultaneous filters AND together. Empty filters return all stories.
- **Files:** `/home/mikey/storyhook/src/tui/data.rs` or `/home/mikey/storyhook/src/tui/state.rs`
- **Complexity:** M

### Task 4.3: Wire Filter Bar into Board View
- **Description:** Update the board rendering layout to include the filter bar above the board table when viewing the Board. Route `/` key to `Action::FocusFilterBar`. When filter bar is focused, route key events to it. `Esc` from filter bar unfocuses and returns to board. Apply active filters to board rendering.
- **Acceptance:** Manual test: pressing `/` focuses filter bar. Typing `state:todo` and Enter shows only todo stories. Adding `priority:high` further narrows results. Backspace removes chips. Board updates in real-time as filters change.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`, `/home/mikey/storyhook/src/tui/components/board.rs`
- **Complexity:** M

### Task 4.4: Dashboard Component
- **Description:** Implement `src/tui/components/dashboard.rs`. Shows: project name (from prefix), total story count, stories per state (bar chart or counts), priority distribution, top-5 ready stories (sortable by priority), recent activity (last 5 updated stories with timestamps). `j`/`k` navigates between sections. `Enter` on a story drills into detail modal. `Enter` on "View Board" navigates to board. `n` opens create form.
- **Acceptance:** Manual test: dashboard shows accurate metrics matching `story summary` output. Navigation works. Drilling into a story opens detail modal. "View Board" navigates to board view.
- **Files:** `/home/mikey/storyhook/src/tui/components/dashboard.rs`
- **Complexity:** M

### Task 4.5: View Switching
- **Description:** Ensure `1` key switches to Dashboard, `2` key switches to Board (global keybindings). When switching views, preserve board cursor position and filter state. Dashboard is the initial view on startup.
- **Acceptance:** `1` and `2` switch views. Board state (cursor, collapsed sections, filters) persists across view switches. Dashboard is shown on startup.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`
- **Complexity:** S

### Task 4.6: Wave 4 Verification
- **Description:** Full test: launch TUI, dashboard shows metrics, navigate to board via `2`, apply filters, verify filtering, switch back to dashboard via `1`, verify board state preserved. Run `cargo test`, `cargo clippy`.
- **Acceptance:** All views functional. Filtering works. View switching preserves state. All tests pass.
- **Files:** None (verification only)
- **Complexity:** S

---

## Wave 5 -- Help Overlay + Mouse Support (Phase 1) + Polish

**Goal:** Help overlay shows keybinding reference. Mouse clicks and scroll work throughout the application. Visual polish: proper alignment, edge cases, resize handling.

**Resumption state after Wave 5:** Feature-complete for Phase 1 (no drag-and-drop). All keyboard and mouse interactions from DESIGN.md Phase 1 work. Help overlay available.

**Depends on:** Wave 4 complete.

### Task 5.1: Help Overlay Component
- **Description:** Implement `src/tui/components/help.rs`. Full keybinding reference organized by context (Global, Board, Filter Bar, Story Detail, Create Form). Scrollable if content exceeds modal height. `Esc`/`q`/`?` closes. `j`/`k` scrolls.
- **Acceptance:** `?` opens help overlay with all keybindings from DESIGN.md. Scrolling works. Closing works via all three keys.
- **Files:** `/home/mikey/storyhook/src/tui/components/help.rs`
- **Complexity:** S

### Task 5.2: Mouse Support -- Board Clicks + Scroll
- **Description:** Implement `Component::handle_mouse()` for Board. Use hit regions populated during render. Left click on story row selects it. Double-click opens detail. Click on section header selects it. Scroll wheel scrolls the board.
- **Acceptance:** Manual test: clicking a story row selects it (cursor moves). Double-clicking opens detail modal. Scroll wheel scrolls the board when content overflows.
- **Files:** `/home/mikey/storyhook/src/tui/components/board.rs`
- **Complexity:** M

### Task 5.3: Mouse Support -- Modal Dismiss + Filter Bar
- **Description:** Implement mouse click handling at the app level: clicking outside a modal closes it. Clicking on the filter bar area focuses it. Clicking on a filter chip removes that filter.
- **Acceptance:** Manual test: clicking outside story detail closes it. Clicking filter bar focuses it. Clicking a filter chip removes it.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`, `/home/mikey/storyhook/src/tui/components/filter_bar.rs`
- **Complexity:** M

### Task 5.4: Mouse Event Routing
- **Description:** Update the main event loop to route `Event::Mouse` events. First check if a modal is open -- if so, check if click is inside modal area (route to modal) or outside (close modal). If no modal, route to the active view component. Pass `MouseEvent` through to component `handle_mouse()`.
- **Acceptance:** Mouse events reach the correct component. Modal dismiss on outside click works. Board mouse navigation works. No mouse events leak to components behind modals.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`
- **Complexity:** M

### Task 5.5: Resize Handling
- **Description:** Handle `Event::Resize(cols, rows)` in the main loop. Update `terminal_size` in AppState. Components should re-render correctly at any size. Test with very small terminal sizes (graceful degradation -- show truncated content, not panics).
- **Acceptance:** Resizing the terminal re-renders correctly. Very small terminal (e.g., 40x10) does not panic -- content is truncated but application remains functional.
- **Files:** `/home/mikey/storyhook/src/tui/app.rs`, all component render methods
- **Complexity:** S

### Task 5.6: $EDITOR Integration for Story Detail
- **Description:** In story detail, `Ctrl+E` opens `$EDITOR` with the story's comments/description in a temp file. On editor exit, parse the temp file and apply changes. Requires suspending the TUI (restore terminal, spawn editor process, re-init terminal on return).
- **Acceptance:** `Ctrl+E` opens the configured editor. After saving and closing, changes are applied to the story. If `$EDITOR` is not set, show a notification message.
- **Files:** `/home/mikey/storyhook/src/tui/components/story_detail.rs`, `/home/mikey/storyhook/src/tui/terminal.rs`
- **Complexity:** M

### Task 5.7: Visual Polish Pass
- **Description:** Review and polish: alignment of columns in board view, proper truncation of long titles with ellipsis, consistent padding, modal overlay dim effect (if achievable with ANSI 16), empty state messages ("No stories found" when filtered list is empty, "No stories yet -- press n to create" when project has no stories). Ensure all disclosure triangles, cursor markers, and badges render correctly.
- **Acceptance:** Manual review: no visual artifacts, alignment is consistent, truncation works, empty states show helpful messages, overall appearance matches DESIGN.md mockup.
- **Files:** All component files
- **Complexity:** M

### Task 5.8: Wave 5 Verification
- **Description:** Complete feature test: all keyboard shortcuts from DESIGN.md work. Mouse interactions (Phase 1) work. Help overlay shows all bindings. Resize works. `$EDITOR` integration works. Run `cargo test`, `cargo clippy`.
- **Acceptance:** All Phase 1 features from DESIGN.md are implemented and functional. All tests pass. No clippy warnings.
- **Files:** None (verification only)
- **Complexity:** S

---

## Wave 6 -- Integration Testing + Edge Cases + Release

**Goal:** Comprehensive testing, edge case handling, and release preparation. The TUI is robust and ready for daily use.

**Resumption state after Wave 6:** TUI is production-ready. All requirements from IDEA.md are implemented (except Phase 2 drag-and-drop which is deferred).

**Depends on:** Wave 5 complete.

### Task 6.1: Integration Tests
- **Description:** Write integration tests that exercise the full TUI data path without actually launching the terminal (test DataStore + Action dispatch + state transitions). Test scenarios: empty project (no stories), large project (50+ stories), concurrent access (CLI writes while TUI is "running" -- verify refresh picks up changes), all mutation types.
- **Acceptance:** Test suite covers: empty project, create/edit/move/comment workflows, concurrent write detection via file watcher, filter + navigate + drill-down sequences.
- **Files:** `/home/mikey/storyhook/tests/tui_integration.rs` or `/home/mikey/storyhook/src/tui/tests.rs`
- **Complexity:** M

### Task 6.2: Edge Case Hardening
- **Description:** Handle edge cases: story deleted outside TUI (detail modal open for deleted story), state removed from states.toml while TUI is running (board references nonexistent state), lock timeout during write (show error, don't corrupt state), extremely long story titles, stories with many labels, deeply nested relationships, empty states.toml (validation error).
- **Acceptance:** Each edge case is handled gracefully (error notification or graceful fallback, never a panic or corrupt state). At least 5 edge cases have explicit tests.
- **Files:** Various component and app files
- **Complexity:** M

### Task 6.3: Performance Check
- **Description:** Test TUI performance with a project containing 100+ stories. Verify render time is acceptable (<16ms per frame for 60fps feel). If slow, identify bottleneck (likely DataStore::load or render). Optimize if needed (cache DataStore, only re-render dirty regions).
- **Acceptance:** Subjective: TUI feels responsive with 100+ stories. No perceptible lag when navigating. DataStore::load completes in <100ms for 100 stories.
- **Files:** Optimization targets TBD
- **Complexity:** S (likely no changes needed, M if optimization required)

### Task 6.4: Documentation Update
- **Description:** Update the `.storyhook/CLAUDE.md` template in storage.rs to mention `story tui`. Add TUI keybinding reference to help topics if applicable. Ensure `story help` or `story --help` mentions the `tui` subcommand.
- **Acceptance:** `story --help` mentions `tui`. The generated CLAUDE.md template references `story tui` as an available command.
- **Files:** `/home/mikey/storyhook/src/storage.rs`, `/home/mikey/storyhook/src/cli.rs`, `/home/mikey/storyhook/src/help_topics.rs`
- **Complexity:** S

### Task 6.5: Final Verification + Semver Bump
- **Description:** Complete end-to-end walkthrough of all features. Run full test suite. Verify all IDEA.md requirements are met. Suggest semver bump (minor -- new feature, no breaking changes).
- **Acceptance:** All IDEA.md requirements are verified. All tests pass. Version bumped.
- **Files:** None (verification only)
- **Complexity:** S

---

## Progress

### Wave 0 -- NOT STARTED
- [ ] Task 0.1: Add TUI dependencies to Cargo.toml
- [ ] Task 0.2: Create TUI module skeleton
- [ ] Task 0.3: Wire `story tui` subcommand in main.rs
- [ ] Task 0.4: Terminal init/restore
- [ ] Task 0.5: Minimal event loop
- [ ] Task 0.6: Wave 0 verification

### Wave 1 -- NOT STARTED
- [ ] Task 1.1: Action enum + View enum
- [ ] Task 1.2: AppState struct
- [ ] Task 1.3: Theme module
- [ ] Task 1.4: Focus management
- [ ] Task 1.5: DataStore bridge
- [ ] Task 1.6: Event system (3 threads + mpsc)
- [ ] Task 1.7: Keymap module
- [ ] Task 1.8: Upgrade main loop to mpsc architecture
- [ ] Task 1.9: Component trait definition
- [ ] Task 1.10: Wave 1 verification

### Wave 2 -- NOT STARTED
- [ ] Task 2.1: Board component -- data model + flat row index
- [ ] Task 2.2: Board component -- rendering
- [ ] Task 2.3: Board component -- keyboard navigation
- [ ] Task 2.4: Status bar component
- [ ] Task 2.5: Wire board + status bar into main loop
- [ ] Task 2.6: Write path -- move story
- [ ] Task 2.7: Wave 2 verification

### Wave 3 -- NOT STARTED
- [ ] Task 3.1: Modal rendering infrastructure
- [ ] Task 3.2: Story detail component -- viewing mode
- [ ] Task 3.3: Story detail component -- editing mode
- [ ] Task 3.4: Write paths -- all story mutations
- [ ] Task 3.5: Create form component
- [ ] Task 3.6: Wire modals into focus stack + main loop
- [ ] Task 3.7: Wave 3 verification

### Wave 4 -- NOT STARTED
- [ ] Task 4.1: Filter bar component
- [ ] Task 4.2: Filter application logic
- [ ] Task 4.3: Wire filter bar into board view
- [ ] Task 4.4: Dashboard component
- [ ] Task 4.5: View switching
- [ ] Task 4.6: Wave 4 verification

### Wave 5 -- NOT STARTED
- [ ] Task 5.1: Help overlay component
- [ ] Task 5.2: Mouse support -- board clicks + scroll
- [ ] Task 5.3: Mouse support -- modal dismiss + filter bar
- [ ] Task 5.4: Mouse event routing
- [ ] Task 5.5: Resize handling
- [ ] Task 5.6: $EDITOR integration for story detail
- [ ] Task 5.7: Visual polish pass
- [ ] Task 5.8: Wave 5 verification

### Wave 6 -- NOT STARTED
- [ ] Task 6.1: Integration tests
- [ ] Task 6.2: Edge case hardening
- [ ] Task 6.3: Performance check
- [ ] Task 6.4: Documentation update
- [ ] Task 6.5: Final verification + semver bump

---

## Parallelism Guide

Within each wave, these tasks can be worked on simultaneously by independent agents:

| Wave | Parallel Groups |
|------|----------------|
| 0 | [0.1, 0.2] can be parallel; 0.3 depends on 0.2; 0.4, 0.5 depend on 0.1+0.2 |
| 1 | [1.1, 1.2, 1.3, 1.4, 1.9] are independent; [1.5, 1.6] are independent of each other but need 0.x; 1.7 depends on 1.1; 1.8 depends on all others |
| 2 | [2.1] first; [2.2, 2.3] depend on 2.1 but are independent of each other; [2.4] independent; [2.5, 2.6] depend on 2.1-2.4 |
| 3 | [3.1] first; [3.2, 3.5] depend on 3.1 but independent of each other; 3.3 depends on 3.2; [3.4] independent of components; 3.6 depends on all |
| 4 | [4.1, 4.2, 4.4] are independent; 4.3 depends on 4.1+4.2; 4.5 depends on 4.4 |
| 5 | [5.1, 5.2, 5.5, 5.6] are independent; [5.3, 5.4] depend on 5.2; 5.7 depends on all |
| 6 | [6.1, 6.2, 6.3, 6.4] are mostly independent; 6.5 depends on all |

---

## Deviations Log

| Task | Original Plan | Actual | Reason |
|------|--------------|--------|--------|
| (none yet) | | | |

---

## Requirement Traceability

| Requirement (from IDEA.md) | Task(s) | Status |
|---------------------------|---------|--------|
| Dashboard home screen with project summary, metrics, navigation | 4.4, 4.5 | Pending |
| Kanban board view with one column per state (grouped table per DESIGN.md) | 2.1, 2.2, 2.3, 2.5 | Pending |
| Rich story cards (ID, title, priority, labels, assignee, blocked) | 2.2 | Pending |
| Story detail modal (~66% screen) with inline editing | 3.1, 3.2, 3.3, 3.6 | Pending |
| Move stories between states via keyboard | 2.3, 2.6 | Pending |
| Move stories via mouse drag-and-drop | DEFERRED (Phase 2) | Deferred -- per DESIGN.md |
| Create new stories via form modal | 3.5, 3.4, 3.6 | Pending |
| Persistent filter bar (search, state, assignee, priority, label, blocked/ready/stale) | 4.1, 4.2, 4.3 | Pending |
| Mouse/trackpad support (click, scroll) | 5.2, 5.3, 5.4 | Pending |
| Built in Rust, integrated into storyhook codebase | 0.1, 0.2, 0.3 | Pending |
| Reads storyhook data directly (JSONL + SQLite) | 1.5 | Pending |
| `story tui` subcommand launches TUI | 0.3 | Pending |
| Dashboard shows project summary with navigation | 4.4 | Pending |
| Stories can be created, edited, moved, filtered within TUI | 3.3, 3.4, 3.5, 2.6, 4.1-4.3 | Pending |
| Keyboard navigation complete (every action without mouse) | 2.3, 3.2, 3.3, 3.5, 4.1, 5.1 | Pending |
| Respect storyhook file locking | 2.6, 3.4 | Pending |
| Work with event-sourced data model (no schema changes) | 1.5 (DataStore reads existing format) | Pending |
| ANSI 16 colors, $NO_COLOR support | 1.3 | Pending |
| Auto-refresh on file changes | 1.6 | Pending |
| Help overlay (keybinding reference) | 5.1 | Pending |
| $EDITOR escape hatch | 5.6 | Pending |

### Deferred Requirements

| Requirement | Reason | Target |
|------------|--------|--------|
| Mouse drag-and-drop between states | Explicitly deferred to Phase 2 in DESIGN.md. Complex (~500-1000 LOC). Ship keyboard-first. | Future wave / separate plan |
| Dependency graph view in terminal | Listed as open question in IDEA.md, not designed in DESIGN.md | Future feature |
| Color theme configuration | DESIGN.md mentions potential project.toml config but does not design it | Future feature |

---

## Complexity Summary

| Complexity | Count | Examples |
|-----------|-------|---------|
| S (Small) | 18 | Dependencies, skeleton, theme, status bar, help, verification tasks |
| M (Medium) | 17 | DataStore, event system, board navigation, modals, filter, write paths |
| L (Large) | 2 | Board rendering (2.2), Story detail editing (3.3) |
| **Total** | **37 tasks** | |

## Estimated Effort

- **Wave 0:** 1 agent session (all tasks are small, sequential)
- **Wave 1:** 2-3 agent sessions (infrastructure, several medium tasks)
- **Wave 2:** 2-3 agent sessions (board is the most complex view)
- **Wave 3:** 2-3 agent sessions (modals + all write paths)
- **Wave 4:** 2 agent sessions (filter bar + dashboard)
- **Wave 5:** 2-3 agent sessions (mouse, help, polish)
- **Wave 6:** 1-2 agent sessions (testing + hardening)

**Total: approximately 11-17 agent sessions**
