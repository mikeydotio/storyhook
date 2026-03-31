# Domain Research: Terminal-Based Kanban Board and Project Management TUIs

Research conducted 2026-03-28.

## Existing Solutions

### Tier 1: Feature-Rich

**rust-kanban (Rust/Ratatui)** — ~300 stars, 27K SLoC
Mouse drag-and-drop, command palette (Ctrl+Shift+P), 8 themes, undo/redo, tag filtering. Built on ratatui 0.29. Most directly comparable — demonstrates drag-and-drop IS achievable with crossterm.

**kanban-tui by Zaloog (Python/Textual)** — ~500 stars, most actively maintained
Multiple backends (SQLite, Jira, Claude), task dependencies with blocking/circular detection, mouse drag-and-drop, MCP server mode, visual charts, customizable columns. Best multi-backend example.

**kanban-tui by sokinpui (Rust)** — ~100 stars
Deep vim modal editing (Normal/Visual/Search/Command), nested boards, meta-boards, fuzzy finder, undo/redo with `.` repetition, visual mode for multi-card operations. Strongest vim-style keyboard navigation.

**taskell (Haskell)** — ~1.8K stars, maintenance-only
Markdown file storage, Trello/GitHub import, subtasks, due dates, vim keybindings. Effectively abandoned. Demonstrates single-maintainer burnout risk.

### Tier 2: Agent-Focused

**kanban-md (Go)** — ~96 stars, active
Built for multi-agent workflows. Atomic `pick --claim`, auto-expiring claims, WIP limits, flow metrics, per-agent skills (CLAUDE.md, CODEX.md). TUI is read-only. Most directly competitive in agent-focused space.

**Cainban (Go/Bubble Tea)** — CLI + TUI + MCP server triple interface. Demonstrates the triple-play storyhook is also targeting.

### Key Observations
- No existing Rust/ratatui kanban has full mouse drag-and-drop + rich cards + filtering + modals
- The agent-focused project tracker space is emerging but fragmented
- Terminal kanban tools work for personal use; team adoption is the unsolved problem (storyhook sidesteps this by being agent-first)

## Established Patterns

### Keyboard Navigation (Universal Convention)
| Pattern | Convention |
|---------|-----------|
| Column navigation | `h`/`l` |
| Card navigation | `j`/`k` |
| Move card between columns | `H`/`L` (shifted) or `>`/`<` |
| Reorder within column | `J`/`K` (shifted) |
| Create card | `n` |
| Edit card | `e` or `Enter` |
| Search/filter | `/` |
| Help | `?` |
| Quit | `q` |

### Column Layout
Equal-width columns with horizontal scroll fallback for many-column scenarios. Minimum column width ~20 characters.

### Card Rendering
2-4 lines per card. Title (truncated), priority (color-coded dots), labels (colored badges), assignee (dimmed), blocked indicator (icon).

### Detail View
Modal overlay (50-80% screen) preserves spatial context. Better than full-screen switch.

## Design Insights from Prior Art
1. **Shifted keys for card movement** (`H`/`L` between columns) is universal — adopt it
2. **`>` / `<` for cycling status** (bradmca) is elegant for sequential workflows
3. **Command palette** (rust-kanban, Ctrl+Shift+P) provides discoverability
4. **Tag color mapping** — predefined colors for common labels reduces cognitive load
5. **External editor** (`$EDITOR` via Ctrl+E) is essential for description editing
6. **Auto-refresh on file change** is critical when CLI and TUI run concurrently
7. **Status bar with mode indicator** keeps users oriented

## Trade-offs
| Decision | Option A | Option B |
|----------|----------|----------|
| Mouse drag-and-drop | Implement from scratch (high effort) | Keyboard-only first, mouse later |
| Column overflow | Horizontal scroll | Visibility toggles |
| Text editing | tui-textarea widget | $EDITOR escape hatch |
| Theme | Fixed theme | Configurable themes |
| Data refresh | File watch (notify crate) | Manual refresh (F5/r) |
| Binary | Subcommand (`story tui`) | Separate binary |
