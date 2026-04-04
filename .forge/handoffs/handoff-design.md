# Handoff: Design → Plan

## Step Completed
design

## Artifacts Produced
- `.forge/DESIGN.md` — Full architecture design, all sections approved by user

## Key Decisions
- **Default type instead of untyped** — `story_type: Option<String>` in snapshot, but `None` maps to default type (first entry in `types.toml`, e.g., "story") at display time. Every story has a displayable type. "Clearing" a type = setting to default. No `StoryTypeCleared` event needed.
- **Reserve "none" as forbidden slug** — Prevents collision with `--type none` filter meaning "show untyped/default-type stories".
- **StoryTypeSet event** — Mirrors `StoryPrioritySet` exactly. Additive, non-breaking.
- **Parent skip in Next handler only** — `is_ready()` unchanged. `story next` filters out `has_children()` stories. `story list --ready` still shows parents.
- **Progress rollup: direct children, superstate CLOSED, includes archived** — Computed in `build_story_views`, attached to `StoryView`.
- **Epic subcommand is sugar** — create → new + type set, add → relate parent-of, list → list filter + progress, show → story show.
- **No conditional agents** — Core team only per TEAM.md.
- **All sections approved** — Architecture overview, data model, CLI design, progress rollup, MCP tools, cross-cutting concerns, design decisions.

## Context for Next Step

### Component Count and Responsibilities
6 files modified, 0 new files:
1. `domain.rs` — Event variant, TypeDef, snapshot field, fold, has_children, compute_progress
2. `storage.rs` — types.toml config lifecycle (load/save/add/remove/ensure/default)
3. `cli.rs` — TypeAction/EpicAction enums, Invocation variants, --type flag, parsers
4. `app.rs` — Command handlers, validation, filtering, parent skip, progress, doctor
5. `mcp.rs` — story_type param on create/update/list tools
6. `output.rs` — StoryView.progress, type + progress rendering

### Interface Contracts
- `StoryEvent::StoryTypeSet { at: String, story_type: String }`
- `TypeDef { slug: String, description: Option<String> }`
- `StorySnapshot.story_type: Option<String>` with `#[serde(default)]`
- `ProgressRollup { children_done: usize, children_total: usize }`
- `has_children(story) -> bool` — checks parent-of relationships
- `compute_progress(story, all_stories) -> Option<ProgressRollup>`
- `load_types`, `load_type_map`, `save_types`, `add_type`, `remove_type`, `ensure_types_file`, `default_type`

### Patterns to Follow
- `StoryPrioritySet` → `StoryTypeSet` (domain.rs:184-187)
- `states.toml` loading → `types.toml` loading (storage.rs:277-344)
- `StateAdd`/`StateRemove` → `TypeAdd`/`TypeRemove` (app.rs:72-88)
- `story phase` → `story type`/`story epic` (cli.rs dispatch)
- `--state`/`--priority` filter → `--type` filter (cli.rs:507, app.rs)
- `doctor` integrity → type integrity check (app.rs:2187-2204)

### Complexity Areas
- `build_story_views` integration for progress rollup — needs full story map (open + archived) before computing
- `epic create` two-event pattern (StoryCreated + StoryTypeSet) inside single lock
- Default type resolution at display time — `None` → first types.toml entry
- MCP create tool: must emit StoryTypeSet after create, then return updated snapshot

### Inter-Component Dependencies
- `domain.rs` is innermost — change first, no deps
- `storage.rs` depends on `domain::TypeDef`
- `cli.rs` depends on nothing (just parsing)
- `app.rs` depends on all three above
- `mcp.rs` depends on `cli::Invocation` and `app::run`
- `output.rs` depends on `domain::ProgressRollup`

## Pipeline State
- Fix cycle: 0 / 3
- Yolo mode: false
- Team roster: core only (no UX designer, security researcher, or accessibility engineer)
- ESCALATE stories pending: 0

## Open Questions for Planning
1. Should `story summary` and `story context` include type breakdown?
2. Should `story export` include `types.toml` in export format?
3. Should `story decompose` accept type annotations in specs?
4. TUI type display — deferred, track as future work
