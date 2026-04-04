# Work Handoff

## Session Summary
- **Session**: session-execute-005
- **Stories completed**: 1 (SH-7)
- **Stories attempted**: 1
- **Status**: Session limit reached (1/1 max_stories_per_session)

## What Happened
Implemented SH-7 (app.rs — Type and Epic command handlers). Generator-evaluator loop: pass on first attempt. All tests pass (383+ unit + integration). No new clippy warnings. Full autonomy (canary complete in prior sessions).

## Stories Completed This Session
- SH-7: app.rs — Type and Epic command handlers — Replaced stub Type/Epic handlers with full implementations. Type List/Add/Remove delegate to storage functions. Epic Create emits StoryCreated + StoryTypeSet in single lock. Epic Add replicates Relate parent-of logic. Epic List filters to type "epic". Epic Show delegates to story_view_by_id. New handler validates type via load_type_map and writes StoryTypeSet. SetFields validates and emits StoryTypeSet. List handler gains --type filter (including --type none for untyped stories). Unknown types return AppError::Validation with available types listed.

## Current Blockers
None.

## Working Context

### Patterns Established
- `StoryTypeSet` follows `StoryPrioritySet` pattern exactly
- `TypeDef` mirrors `StateDef` pattern
- `ProgressRollup { children_done: usize, children_total: usize }` — defined but progress field always None until SH-8 populates it
- types.toml follows exact same pattern as states.toml
- `ensure_types_file` is the lazy auto-creation mechanism
- `add_type` checks "none" reserved slug BEFORE loading types
- `remove_type` uses `load_all_snapshots` (open + archived) to check for in-use types
- `StoryView.progress: Option<ProgressRollup>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`
- render_story shows type after priority line: `type: {value}` or `type: -` when None
- List format: `{id} [{state}]{priority}{type_badge} {title}{progress_summary}{labels}{flagged}{stale}`
- **NEW**: Type handlers follow StateAdd/StateRemove pattern (ensure_project + with_project_lock)
- **NEW**: Epic Create validates "epic" type exists in types.toml before creating
- **NEW**: Epic Add reuses validate_parent_constraints + relation_edges + has_relation pattern from Relate handler
- **NEW**: Epic List uses build_story_views with retain filter, same as existing List handler
- **NEW**: Type validation at write time (New, SetFields) uses `storage::load_type_map(root)` and returns `AppError::Validation` with available types listed
- **NEW**: `--type none` in List filters to `story_type.is_none()`
- **NEW**: `--type <slug>` in List filters to `story_type.as_deref() == Some(slug)`
- **NEW**: SetFields story_type handling placed after `unblocked` block, before `json_patch` block

### Micro-Decisions
- Type fallback in render_story uses "-" (consistent with assignee pattern), not "story" default — output.rs has no storage access; default resolution is SH-8 responsibility
- Progress line only shown when progress is Some (not shown for non-parent stories)
- Type badge in list is empty when story_type is None (no badge at all, not "[?]" or "[story]")
- progress_summary uses compact format (n/m) in list, verbose format (n/m children done (Z%)) in detail view
- **NEW**: Epic Create uses `story_view_response` (same as New handler) to return full story view with derived relationships
- **NEW**: Epic Add returns `story_view_by_id(root, &epic_id)` — shows the epic after adding child
- **NEW**: Epic Create does NOT fire event hooks for the create — only the New handler has hook firing. This is acceptable since epic create is sugar.
- **NEW**: Type validation error message includes available types: `"unknown type \`{st}\`. Available types: bug, chore, epic, story, task"`

### Code Landmarks
- `src/app.rs:4` — imports now include TypeAction and EpicAction
- `src/app.rs:45-66` — Invocation::New with story_type extraction, validation, and StoryTypeSet emission
- `src/app.rs:119-122` — Invocation::List with story_type extraction
- `src/app.rs:174-182` — --type filter in List handler (after phase filter)
- `src/app.rs:1736-1739` — Invocation::SetFields with story_type extraction
- `src/app.rs:1816-1832` — story_type validation and StoryTypeSet emission in SetFields
- `src/app.rs:1948-2061` — Full Type and Epic handler implementations
- `src/app.rs:1951-1976` — Type List/Add/Remove handlers
- `src/app.rs:1978-1998` — Epic Create (two-event pattern in single lock)
- `src/app.rs:2000-2049` — Epic Add (parent-of relationship delegation)
- `src/app.rs:2051-2056` — Epic List (filter to epic type)
- `src/app.rs:2058-2061` — Epic Show (delegates to story_view_by_id)

### Test State
- **Command**: `cargo test`
- **Result**: 383+ unit tests + integration tests, 0 failures
- **Clippy**: 23 pre-existing warnings, 0 new
- **Flaky tests**: none detected

## What's Next
- SH-8 (T3.1: app.rs — build_story_views progress rollup, Next parent skip, doctor type check) is now unblocked
- SH-9 (T3.2: mcp.rs — story_type param on MCP tools) is now unblocked
- SH-10 (T3.3: storage.rs + app.rs — Export/import types.toml) is now unblocked
- SH-8, SH-9, SH-10 are all wave 4 (independent of each other)
- SH-11 is blocked by SH-8, SH-9, SH-10
- Recommended next: story next will pick one of SH-8/SH-9/SH-10 based on priority
