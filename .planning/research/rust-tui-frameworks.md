# Domain Research: Rust TUI Frameworks for a Kanban Board TUI

Research conducted 2026-03-28 for the storyhook TUI.

## Executive Summary

**ratatui** is the clear winner. It is the de facto standard Rust TUI framework with 21M+ crates.io downloads, active maintenance (v0.30.0), and a thriving ecosystem. No other Rust TUI framework comes close. The main engineering challenges will be implementing drag-and-drop (feasible but custom) and choosing the right architecture pattern.

## 1. Ratatui — Current State

| Property | Value |
|----------|-------|
| Latest version | v0.30.0 |
| crates.io downloads | 21.7M total |
| Dependents | 2,100+ crates |
| License | MIT |
| Backend | crossterm (default), termion, termwiz |
| Rendering model | Immediate-mode (redraw entire UI each frame) |
| Notable adopters | Netflix, OpenAI, AWS, Vercel |

### Key v0.30.0 Changes
- Modular workspace: split into `ratatui-core`, `ratatui-widgets`, `ratatui-crossterm`, etc.
- `ratatui::run()` helper for simplified app bootstrapping
- `no_std` support

### Built-in Widgets
Block, Paragraph, List, Table, Tabs, Gauge, LineGauge, BarChart, Sparkline, Chart, Canvas, Scrollbar, Clear.

### What Ratatui Does NOT Provide
- No event loop (bring your own via crossterm)
- No focus management
- No mouse hit-testing
- No text input widget
- No drag-and-drop abstraction

## 2. Alternative Frameworks

### Cursive (0.21.1)
Higher-level API with built-in views, event loop, focus, and mouse handling. Much smaller ecosystem. Less flexible. **Verdict:** Viable for simpler TUIs, but ratatui's ecosystem wins for complex apps.

### rat-salsa
Event-queue framework on top of ratatui with tasks, timers, focus handling, dialog windows. Includes `rat-widget` (comprehensive widget library) and `rat-focus`. Opinionated — takes over event loop. Single maintainer.

### tui-realm
Elm+React component model on top of ratatui. Adds abstraction overhead. Smaller community.

## 3. Ratatui Ecosystem — Companion Crates

### Tier 1: Production-Ready
| Crate | Purpose | Confidence |
|-------|---------|------------|
| `ratatui-macros` | Layout/constraint shorthand, text styling macros | **HIGH** |
| `tui-textarea` | Multi-line text editor with undo/redo, search | **HIGH** |
| `tui-scrollview` | Scrollable container widget | **HIGH** |
| `tui-popup` | Popup/modal overlay widget | **HIGH** |
| `tui-input` | Headless single-line input state management | **HIGH** |
| `tachyonfx` | Animation/transition effects (50+ effects) | **MEDIUM** |

### Tier 2: Solid
| Crate | Purpose | Confidence |
|-------|---------|------------|
| `rat-widget` | Comprehensive widget suite (text-input, date, checkbox, radio, slider, table, file dialog, menu bar) | **MEDIUM** |
| `rat-focus` | Focus handling via FocusFlag, Tab/Shift-Tab navigation | **MEDIUM** |
| `ratatui-interact` | Focus management, ClickRegion hit-testing, interactive widgets | **MEDIUM** |
| `ratatui-themes` | 15+ popular color themes (Dracula, Nord, Catppuccin, etc.) | **MEDIUM** |

## 4. Mouse Support

### How It Works
Mouse events come from crossterm: `Event::Mouse(MouseEvent { kind, column, row, modifiers })`.

Supported: `Down(button)`, `Up(button)`, `Drag(button)`, `Moved`, `ScrollDown/Up/Left/Right`.

### Drag-and-Drop Feasibility
**Feasible but requires custom implementation.** No existing crate provides it.

Approach:
1. `MouseEventKind::Down` → check if within a card's Rect, enter "dragging" state
2. `MouseEventKind::Drag` → update visual feedback, map position to target column
3. `MouseEventKind::Up` → execute move, exit dragging state

Challenges: hit-testing (track widget Rects), visual feedback during drag, terminal compatibility (some terminals don't report button for Up/Drag).

## 5. Architecture Patterns

### Pattern A: Elm Architecture (TEA)
Model + Update (message → state transition) + View (state → UI). Good for medium-complexity. All state centralized.

### Pattern B: Component Architecture
Trait-based components with `handle_events()`, `update()`, `render()`. Co-locates related logic. Scales to larger apps.

### Recommendation: Hybrid TEA + Component
- TEA at top level: centralized `AppState` with message-based transitions
- Component trait for views: `DashboardView`, `BoardView`, `StoryDetailModal`, `FilterBar`
- Components handle own rendering and local UI state
- Components emit Actions upward for state mutations

```
src/
  tui/
    main.rs
    app.rs          -- AppState + message dispatch
    action.rs       -- Action enum
    terminal.rs     -- Terminal setup/teardown
    event.rs        -- Event polling (crossterm)
    components/
      mod.rs         -- Component trait
      dashboard.rs
      board.rs
      story_detail.rs
      filter_bar.rs
      create_story.rs
```

## 6. Technology Recommendations

### Core Stack
| Layer | Recommendation | Confidence |
|-------|---------------|------------|
| TUI framework | `ratatui` v0.30.x | **HIGH** |
| Terminal backend | `crossterm` | **HIGH** |
| Async runtime | `tokio` | **HIGH** |
| Text editing | `tui-textarea` + `tui-input` | **HIGH** |
| Layout helpers | `ratatui-macros` | **HIGH** |
| Scrollable content | `tui-scrollview` | **HIGH** |
| Popups/modals | `tui-popup` + `Clear` widget pattern | **HIGH** |
| Focus management | Custom focus stack (modal push/pop) | **MEDIUM** |
| File watching | `notify` crate | **HIGH** |

### Integration
Subcommand (`story tui` or `story board`), not separate binary. Feature-flag TUI dependencies.
