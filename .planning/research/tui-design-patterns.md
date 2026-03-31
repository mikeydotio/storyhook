# Domain Research: TUI Design Patterns for Ratatui Kanban Board

Research conducted 2026-03-28.

## 1. Modal/Overlay Patterns

**Ratatui renders sequentially — no z-index.** To create a modal:
1. Calculate a centered `Rect` (~66% of terminal area)
2. Render `Clear` widget to erase background content in that area
3. Render modal content (Block with border + inner widgets)
4. Focus trapping: when modal is open, event handler routes only to modal component

```rust
// Centered rect calculation
let area = frame.area();
let modal_width = area.width * 2 / 3;
let modal_height = area.height * 2 / 3;
let x = (area.width - modal_width) / 2;
let y = (area.height - modal_height) / 2;
let modal_rect = Rect::new(x, y, modal_width, modal_height);
```

The `tui-popup` crate wraps this pattern. Consider using it or implementing the pattern directly.

## 2. Form Patterns

### Tab-cycling fields
Maintain a `focused_field: usize` index. Tab increments, Shift+Tab decrements. Render focused field with different border style or color.

### Field types needed for storyhook:
- **Text input** (title): `tui-input` crate — headless state management
- **Text area** (comments): `tui-textarea` crate — multi-line with undo/redo
- **Select/dropdown** (priority, state): Custom — render list of options, up/down to select
- **Multi-select** (labels): Custom — render checkboxes, space to toggle
- **Assignee picker**: Custom — render member list, search-to-filter

### External editor escape hatch
Open `$EDITOR` on a temp file, suspend terminal (`ratatui::restore()`), wait for editor to exit, read temp file, resume terminal (`ratatui::init()`). Essential for long descriptions.

## 3. Drag-and-Drop in Terminals

**Feasible but no off-the-shelf solution exists.** Implementation:

### State machine
```
Idle → (MouseDown on card) → DragPending
DragPending → (MouseDrag, moved > threshold) → Dragging
Dragging → (MouseUp) → Drop (resolve target column)
Dragging → (Esc) → Cancel (return card to origin)
DragPending → (MouseUp, same position) → Click (select card)
```

### Visual feedback during drag
- Highlight source card (dimmed or outlined)
- Highlight target column (border color change)
- Show insertion position indicator in target column
- Cannot do floating ghost card (terminal limitation)

### Hit-testing
Track `Rect` of each card and each column after layout. On mouse event, iterate regions to find which card/column the cursor is in. The `ratatui-interact` crate's `ClickRegion` system can help.

### Platform issues
- Some terminals don't report button for Up/Drag events
- tmux requires `set -g mouse on`
- Drag threshold needed to distinguish click from drag (3-5 cells)

## 4. Filter Bar Patterns

### Persistent filter bar at top of screen
- Fixed-height area (1-2 lines) above the board
- Shows active filter "chips" (e.g., `[priority:high] [label:bug] [assignee:mikey]`)
- Each chip is removable (click or keyboard shortcut)
- Type to add new filter — fuzzy matching against filter categories

### Implementation
- Filter bar is its own component with focus state
- When focused: captures text input, shows autocomplete dropdown
- When unfocused: displays active filters as read-only chips
- Filter state is shared with board component via AppState

## 5. Dashboard Layout Patterns

### Summary statistics
Use ratatui's `BarChart`, `Gauge`, or `Sparkline` widgets for metrics. Key metrics:
- Stories by state (bar chart or table)
- Priority distribution
- Blocked/ready counts
- Stale story count
- Recent activity

### Navigation
Render dashboard sections as selectable "cards" or list items. Enter navigates to the selected view (board, graph, search).

### Layout
Use `Layout::vertical` for main sections, `Layout::horizontal` within sections for side-by-side stats.

## 6. Color and Theming

### Best practices
- Use ANSI 16 named colors as base palette (respects terminal theme)
- Honor `$NO_COLOR` environment variable (https://no-color.org/)
- Make colors configurable via storyhook config
- Avoid emoji as primary UI elements — grapheme width is unpredictable
- Use Unicode box-drawing characters (reliable across terminals)

### Priority colors (suggested)
| Priority | Color | Symbol |
|----------|-------|--------|
| Critical | Red | `!!!` |
| High | Yellow | `!!` |
| Medium | Blue | `!` |
| Low | Gray | `.` |
| None | Default | (none) |

### Label colors
Assign colors from a rotating palette. Or let users configure label→color mapping in storyhook config.

## 7. Common Pitfalls

### Performance
- Ratatui Table widget degrades at 15K+ items (unlikely for storyhook)
- Never do synchronous I/O in the render loop — use background thread + mpsc channel
- Debounce file watch events (100-500ms) to avoid re-reads during rapid CLI operations

### Terminal state
- Since ratatui 0.28.1, `ratatui::init()` automatically installs panic hooks that restore terminal state
- Always use `ratatui::init()` / `ratatui::restore()` rather than manual setup
- When spawning external editor: `ratatui::restore()` → editor → `ratatui::init()`

### Focus management
- No built-in focus system in ratatui
- Simplest: `Focus` enum with `next()`/`prev()` methods
- For modals: push/pop focus context (when modal opens, push; when it closes, pop)
- `rat-focus` crate provides `FocusFlag` per widget for more sophisticated needs

### Unicode
- Use `unicode-width` crate for accurate string width measurement
- Avoid emoji as primary UI elements
- Test with CJK characters if internationalization matters
- ratatui 0.26.3+ has improved Unicode rendering

### Resize
- Handle terminal resize events (`Event::Resize(width, height)`)
- Re-layout everything on resize — ratatui's `Layout` handles this naturally
- Test at common sizes: 80x24, 120x40, 200x60
