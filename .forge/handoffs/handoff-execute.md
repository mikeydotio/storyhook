# Work Handoff

## Session Summary
- **Session**: session-execute-008
- **Stories completed**: 1 (SH-10)
- **Stories attempted**: 1
- **Status**: Session limit reached (1/1 max_stories_per_session)

## What Happened
Implemented SH-10 (export/import types.toml, ImportStory type handling). Generator-evaluator loop: pass on first attempt. All tests pass (389+ unit + integration). No new clippy warnings. Full autonomy (canary complete in prior sessions).

## Stories Completed This Session
- SH-10: storage.rs + app.rs — Export/import types.toml, ImportStory type handling — Added `types: Vec<TypeDef>` with `#[serde(default)]` to `ProjectExport`; `export_project` calls `load_types`; `import_project` restores types.toml via `save_types`; both Import handler and `import_stories_batch` emit `StoryTypeSet` events for typed stories. Two integration tests added (round-trip with custom types, import with story_type).

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
- `has_children` is a standalone public function in domain.rs, NOT a method on HierarchyGraph (which is private)
- `compute_progress` takes `(&StorySnapshot, &BTreeMap<String, StorySnapshot>)` — same pattern as `is_ready`
- `build_story_views` computes `progress_map` BEFORE `stories.into_values()` consumes the BTreeMap
- Next handler combines `is_ready` and `!has_children` in a single `.filter()` call
- `doctor_report` loads type_map once and checks each story's type after iterating flagged_reasons
- MCP update_story story_type handling uses `Invocation::SetFields` (not a dedicated variant) — placed after awaiting, before error fallback
- MCP update_story processes one field per call in priority order: state > priority > labels > assignee > awaiting > story_type
- storyhook_bulk_create story_type works via JSON passthrough (entire object serialized, deserialized by ImportStory)
- **NEW**: ProjectExport.types uses `#[serde(default)]` for backward-compatible deserialization of pre-types exports
- **NEW**: export_project calls `load_types(root)?` and includes types in the ProjectExport struct
- **NEW**: import_project checks `!export.types.is_empty()` before calling `save_types` — skips for legacy exports
- **NEW**: Import handler (app.rs:761-766) and import_stories_batch (app.rs:2826-2831) both emit StoryTypeSet after description/comment handling, following same pattern as priority/labels/assignee

### Micro-Decisions
- Type fallback in render_story uses "-" (consistent with assignee pattern), not "story" default — output.rs has no storage access; default resolution is display-time concern
- Progress line only shown when progress is Some (not shown for non-parent stories)
- Type badge in list is empty when story_type is None (no badge at all, not "[?]" or "[story]")
- progress_summary uses compact format (n/m) in list, verbose format (n/m children done (Z%)) in detail view
- Epic Create uses `story_view_response` (same as New handler) to return full story view with derived relationships
- Epic Add returns `story_view_by_id(root, &epic_id)` — shows the epic after adding child
- Epic Create does NOT fire event hooks for the create — only the New handler has hook firing
- Type validation error message includes available types: `"unknown type \`{st}\`. Available types: bug, chore, epic, story, task"`
- `compute_progress` handles dangling references gracefully — missing children counted as not-done via `map_or(false, ...)`
- Doctor type check placed AFTER the flagged_reasons loop, not inside it — separate concern from integrity issues
- **NEW**: import_project only restores types when export.types is non-empty — avoids overwriting auto-created types.toml when importing legacy exports
- **NEW**: story_type event pushed in same events Vec as priority/labels/assignee — single write_story_events call per story

### Code Landmarks
- `src/domain.rs:108-112` — ProgressRollup struct definition
- `src/domain.rs:520-525` — `has_children` function
- `src/domain.rs:527-556` — `compute_progress` function
- `src/domain.rs:558-580` — `is_ready` function (unchanged)
- `src/app.rs:4-8` — imports now include `compute_progress` and `has_children`
- `src/app.rs:690` — Next handler filter: `is_ready && !has_children`
- `src/app.rs:761-766` — Import handler StoryTypeSet emission
- `src/app.rs:2826-2831` — import_stories_batch StoryTypeSet emission
- `src/app.rs:2231-2236` — `progress_map` computation in `build_story_views`
- `src/app.rs:2261` — `progress: progress_map.get(&story_id).cloned()`
- `src/app.rs:2355` — `let type_map = storage::load_type_map(root)?;` in doctor_report
- `src/app.rs:2364-2368` — unknown type check in doctor_report
- `src/app.rs:1948-2061` — Full Type and Epic handler implementations (from SH-7)
- `src/storage.rs:844-849` — ProjectExport struct with types field
- `src/storage.rs:867` — load_types in export_project
- `src/storage.rs:938-940` — save_types in import_project
- `src/mcp.rs:152` — story_type in list inputSchema
- `src/mcp.rs:178` — story_type in create inputSchema
- `src/mcp.rs:196` — story_type in update inputSchema
- `src/mcp.rs:299` — story_type in bulk_create per-item schema
- `src/mcp.rs:586-598` — story_type handling in update build_invocation (SetFields)

### Test State
- **Command**: `cargo test`
- **Result**: 389+ unit tests + integration tests, 0 failures
- **Clippy**: 25 pre-existing warnings, 0 new
- **Flaky tests**: none detected

## What's Next
- SH-11 (T4.1: Full compilation and test pass) is now unblocked — all blockers (SH-8, SH-9, SH-10) are done
- SH-11 is the final story — after it's done, proceed to review+validate
