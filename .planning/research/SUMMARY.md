# Research Summary: Storyhook TUI

## Stack Decision: ratatui + crossterm (HIGH confidence)

No serious alternative exists for a Rust TUI with full mouse support. ratatui v0.30.0 is the de facto standard with 21M+ downloads, active maintenance, and a mature ecosystem of companion crates.

## Key Ecosystem Crates

| Crate | Purpose | Status |
|-------|---------|--------|
| `ratatui` | Core framework | Required |
| `crossterm` | Terminal backend, mouse events | Required |
| `tokio` | Async runtime for event loop + background I/O | Required |
| `tui-textarea` | Multi-line text editing | Required |
| `tui-input` | Single-line text input | Required |
| `ratatui-macros` | Layout/styling convenience | Recommended |
| `tui-scrollview` | Scrollable content areas | Recommended |
| `tui-popup` | Modal overlays | Recommended |
| `notify` | File watching for auto-refresh | Recommended |

## Architecture: Hybrid TEA + Component (HIGH confidence)

- Centralized `AppState` with enum-based `Action` messages (TEA at top level)
- Component trait for each view: Dashboard, Board, StoryDetail, FilterBar, CreateStory
- Components own local UI state (cursor, scroll, focus within form)
- Components emit Actions upward for data mutations

## Prior Art Findings

### No existing tool matches the full vision
- **rust-kanban** has drag-and-drop but its own data model (not storyhook)
- **kanban-md** is agent-focused but has a read-only TUI
- **taskwarrior-tui** is the best Rust/ratatui TUI reference for maturity

### Universal keyboard conventions to adopt
- `h/j/k/l` for navigation, `H/L` to move cards between columns
- `>/<` to cycle story state (elegant shortcut)
- `n` to create, `e`/`Enter` to edit, `/` to search, `?` for help, `q` to quit
- Command palette (`Ctrl+Shift+P` or `:`) for discoverability

## Critical Design Decisions

### 1. Drag-and-drop: Phase it
Mouse drag-and-drop is feasible (crossterm provides Drag events) but requires ~500-1000 LOC of custom hit-testing and state machine code. **Ship keyboard-only movement first, add drag-and-drop as enhancement.** This is what every prior art project did too — none shipped drag-and-drop in their first version.

### 2. Column overflow: Horizontal scroll
storyhook's states.toml allows arbitrary numbers of states. Equal-width columns work for 3-5 states but break at 6+. Use equal-width with minimum column width, horizontal scroll when columns exceed terminal width.

### 3. Text editing: Both inline and external
Use `tui-textarea` for comments and short text. Provide `$EDITOR` escape hatch (Ctrl+E) for long descriptions. Every successful TUI does this.

### 4. Auto-refresh: File watching
Use `notify` crate to watch `.storyhook/` directory. Debounce to 200ms. Critical because CLI and TUI will run concurrently — user may run `story SH-1 is done` in another terminal.

### 5. Colors: ANSI-first, configurable
Use ANSI 16 named colors as base palette (respects terminal theme). Honor `$NO_COLOR`. Make theme configurable in `.storyhook/project.toml`.

### 6. Integration: Subcommand with feature flag
`story tui` subcommand, not separate binary. Feature-flag TUI dependencies (`--features tui`) so the core CLI stays lean for users who don't want the TUI.

## Risks

| Risk | Mitigation |
|------|-----------|
| Drag-and-drop complexity | Phase it — keyboard-first, mouse later |
| TUI deps bloat binary size | Feature flag |
| Terminal compatibility (mouse) | Test on macOS Terminal, iTerm2, Windows Terminal, Alacritty, tmux |
| Text editing pain | External editor escape hatch |
| Single-maintainer burnout | Keep scope tight — board + dashboard + detail, no extras |
