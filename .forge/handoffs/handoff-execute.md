# Work Handoff

## Session Summary
- **Session**: session-execute-006
- **Stories completed**: 1 (SH-8)
- **Stories attempted**: 1
- **Status**: Session limit reached (1/1 max_stories_per_session)

## What Happened
Implemented SH-8 (app.rs — build_story_views progress rollup, Next parent skip, doctor type check). Generator-evaluator loop: pass on first attempt. All tests pass (389+ unit + integration). No new clippy warnings. Full autonomy (canary complete in prior sessions).

## Stories Completed This Session
- SH-8: app.rs — build_story_views progress rollup, Next parent skip, doctor type check — Added `has_children` and `compute_progress` public functions to domain.rs. Modified `build_story_views` in app.rs to compute a `progress_map` from all stories and attach progress to each StoryView. Added `!has_children(&v.story)` filter to Next handler so parent stories are excluded from next recommendations. Added type integrity check to `doctor_report` that loads `storage::load_type_map` and flags stories with unknown types. Added 6 unit tests and 6 integration tests.

## Current Blockers
None.

## Working Context

### Patterns Established
- `StoryTypeSet` follows `StoryPrioritySet` pattern exactly
- `TypeDef` mirrors `StateDef` pattern
- `ProgressRollup { children_done: usize, children_total: usize }` — defined in domain.rs
- types.toml follows exact same pattern as states.toml
- `ensure_types_file` is the lazy auto-creation mechanism
- `add_type` checks "none" reserved slug BEFORE loading types
- `remove_type` uses `load_all_snapshots` (open + archived) to check for in-use types
- `StoryView.progress: Option<ProgressRollup>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`
- render_story shows type after priority line: `type: {value}` or `type: -` when None
- List format: `{id} [{state}]{priority}{type_badge} {title}{progress_summary}{labels}{flagged}{stale}`
- Type handlers follow StateAdd/StateRemove pattern (ensure_project + with_project_lock)
- Epic Create validates "epic" type exists in types.toml before creating
- Epic Add reuses validate_parent_constraints + relation_edges + has_relation pattern from Relate handler
- Epic List uses build_story_views with retain filter, same as existing List handler
- Type validation at write time (New, SetFields) uses `storage::load_type_map(root)` and returns `AppError::Validation` with available types listed
- `--type none` in List filters to `story_type.is_none()`
- `--type <slug>` in List filters to `story_type.as_deref() == Some(slug)`
- SetFields story_type handling placed after `unblocked` block, before `json_patch` block
- **NEW**: `has_children` is a standalone public function in domain.rs, NOT a method on HierarchyGraph (which is private)
- **NEW**: `compute_progress` takes `(&StorySnapshot, &BTreeMap<String, StorySnapshot>)` — same pattern as `is_ready`
- **NEW**: `build_story_views` computes `progress_map` BEFORE `stories.into_values()` consumes the BTreeMap
- **NEW**: Next handler combines `is_ready` and `!has_children` in a single `.filter()` call
- **NEW**: `doctor_report` loads type_map once and checks each story's type after iterating flagged_reasons

### Micro-Decisions
- Type fallback in render_story uses "-" (consistent with assignee pattern), not "story" default — output.rs has no storage access; default resolution is display-time concern
- Progress line only shown when progress is Some (not shown for non-parent stories)
- Type badge in list is empty when story_type is None (no badge at all, not "[?]" or "[story]")
- progress_summary uses compact format (n/m) in list, verbose format (n/m children done (Z%)) in detail view
- Epic Create uses `story_view_response` (same as New handler) to return full story view with derived relationships
- Epic Add returns `story_view_by_id(root, &epic_id)` — shows the epic after adding child
- Epic Create does NOT fire event hooks for the create — only the New handler has hook firing
- Type validation error message includes available types: `"unknown type \`{st}\`. Available types: bug, chore, epic, story, task"`
- **NEW**: `compute_progress` handles dangling references gracefully — missing children counted as not-done via `map_or(false, ...)`
- **NEW**: Doctor type check placed AFTER the flagged_reasons loop, not inside it — separate concern from integrity issues
- **NEW**: `has_children` and `compute_progress` placed after `compute_integrity_issues` in domain.rs (line 520), before `is_ready`

### Code Landmarks
- `src/domain.rs:108-112` — ProgressRollup struct definition
- `src/domain.rs:520-525` — `has_children` function
- `src/domain.rs:527-556` — `compute_progress` function
- `src/domain.rs:558-580` — `is_ready` function (unchanged)
- `src/app.rs:4-8` — imports now include `compute_progress` and `has_children`
- `src/app.rs:690` — Next handler filter: `is_ready && !has_children`
- `src/app.rs:2231-2236` — `progress_map` computation in `build_story_views`
- `src/app.rs:2261` — `progress: progress_map.get(&story_id).cloned()`
- `src/app.rs:2355` — `let type_map = storage::load_type_map(root)?;` in doctor_report
- `src/app.rs:2364-2368` — unknown type check in doctor_report
- `src/app.rs:1948-2061` — Full Type and Epic handler implementations (from SH-7)

### Test State
- **Command**: `cargo test`
- **Result**: 389+ unit tests + integration tests, 0 failures
- **Clippy**: 25 pre-existing warnings, 0 new
- **Flaky tests**: none detected

## What's Next
- SH-9 (T3.2: mcp.rs — story_type param on MCP tools) is ready
- SH-10 (T3.3: storage.rs + app.rs — Export/import types.toml) is ready
- SH-9 and SH-10 are both wave 4, independent of each other
- SH-11 is blocked by SH-9 and SH-10
- Recommended next: story next will pick one of SH-9/SH-10 based on priority
