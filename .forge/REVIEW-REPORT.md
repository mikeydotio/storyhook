# Review Report

## Summary

The Story Types & Epics implementation is solid, well-structured code that closely follows established patterns in the codebase (states.toml/StateDef as template for types.toml/TypeDef, StoryPrioritySet as template for StoryTypeSet). The architecture is clean, backward compatibility is correctly handled via `#[serde(default)]`, and all integration points (CLI, MCP, export/import, doctor) are wired up. No critical defects were found. There are a few design drift items and usability opportunities worth considering.

## Findings

### 1. Default Type Not Displayed for Untyped Stories in `story show`
- **Severity**: Important
- **Description**: DESIGN.md specifies that `story_type: None` should render as the default type from `types.toml` at display time (e.g., "type: story"). The actual implementation in `render_story` (output.rs:342) renders `None` as `"type: -"`. The list view omits the type badge entirely for untyped stories. This means old/untyped stories show "type: -" instead of "type: story", which diverges from the design intent that "every story has a displayable type."
- **Location**: `src/output.rs:342` (`render_story`) and `src/output.rs:256-259` (list rendering)
- **Option 1 (Recommended)**: Accept the current behavior as a simplification. Displaying "-" is clear, avoids implying the user explicitly chose "story", and avoids needing to load `types.toml` in `output.rs` (which currently has no storage dependency). The JSON output correctly preserves `null` for API consumers. Document that "-" means "untyped / default". -- Pros: Simpler code, no new dependencies in output.rs, honest about what the data actually says. Cons: Diverges from DESIGN.md's stated intent.
- **Option 2**: Pass the default type slug as a parameter to `render_story` and use it as fallback. Requires the caller (app.rs) to load `default_type(root)` and thread it through `StoryView` or as a render parameter. -- Pros: Matches DESIGN.md exactly, every story displays a type. Cons: Adds coupling between output layer and storage, extra I/O per render, may confuse users who didn't explicitly set a type.

### 2. MCP `storyhook_update_story` Silently Drops `story_type` When Combined With Other Fields
- **Severity**: Important
- **Description**: The `storyhook_update_story` MCP handler processes exactly one field per call in a priority chain (state > priority > labels > assignee > awaiting > story_type). If an MCP client sends `{"id": "SH-1", "state": "done", "story_type": "epic"}`, only the state change is applied and the type change is silently dropped. The tool description says "Processes one update field per call" but does not explicitly list `story_type` in that context, making it easy for AI agents to assume both fields will be applied.
- **Location**: `src/mcp.rs:537-601` (`storyhook_update_story` in `build_invocation`)
- **Option 1 (Recommended)**: Update the MCP tool description to explicitly mention that `story_type` is subject to the same one-field-per-call limitation, and list it in the priority order. -- Pros: Zero code change, accurate documentation, follows existing pattern. Cons: Doesn't fix the underlying limitation.
- **Option 2**: For `story_type`, fall through to `SetFields` even when other fields are present, so type changes can be combined with other single-field updates. This would require restructuring the priority chain to accumulate fields. -- Pros: Better MCP UX for agents. Cons: Breaks the one-field-per-call invariant, significant refactor.

### 3. Import Does Not Validate `story_type` Against `types.toml`
- **Severity**: Useful
- **Description**: When importing stories via `story import` (the JSON array import path), `ImportStory.story_type` is written as a `StoryTypeSet` event without validation against `types.toml`. This means `story import` can create stories with types that don't exist in the project's configuration. These will later be flagged by `story doctor`, but the import itself succeeds silently. By contrast, `story new --type` and `story set --type` both validate against `load_type_map`.
- **Location**: `src/app.rs:761-766` (import handler, story_type event emission)
- **Option 1 (Recommended)**: Add a validation pass before the import loop that checks each `ImportStory.story_type` against `load_type_map`, failing early with a clear message. -- Pros: Consistent with write-time validation elsewhere, prevents doctor issues. Cons: Makes import stricter, may break batch imports where types are expected to be added separately.
- **Option 2**: Accept the current behavior. Import is a bulk/migration tool where strictness may be counterproductive. Doctor catches drift. -- Pros: Import stays permissive for migration scenarios. Cons: Inconsistent validation behavior across entry points.

### 4. `remove_type` Does Not Warn About Default Type Removal
- **Severity**: Useful
- **Description**: `storage::remove_type` allows removing the first type in `types.toml` (the default type), which would change which type is displayed for all untyped stories. If the user removes "story" (the default), "epic" becomes the new default. The `default_type` function returns the first entry, so this silently changes the semantics of all untyped stories. There is also no guard against removing all types, which would cause `default_type` to error.
- **Location**: `src/storage.rs:441-462` (`remove_type`)
- **Option 1 (Recommended)**: Add a guard in `remove_type` that prevents removing the last type (returns an error like "cannot remove the last type"). The default-type-shift issue is low-risk since it only affects display of untyped stories, not stored data. -- Pros: Prevents the hard error from `default_type` when types list is empty. Cons: Doesn't address the default-shift case.
- **Option 2**: Also warn (or prevent) removal of the first type in the list, since it serves as the default. -- Pros: Prevents accidental semantics change. Cons: Over-protective; reordering types is a valid use case.

### 5. Epic List Does Not Show Progress Bar From Design Examples
- **Severity**: Useful
- **Description**: DESIGN.md shows `story epic list` output with ASCII progress bars (`████████░░ 80%`), but the actual list rendering shows progress as `(4/5)` without a bar or percentage. This is a cosmetic difference from the design specification's output examples.
- **Location**: `src/output.rs:261-264` (list rendering progress), `src/output.rs:381-389` (single story progress display)
- **Option 1 (Recommended)**: Accept the current compact format. The `(done/total)` display is clear and functional. Progress bars add visual noise in list output and are better suited for the TUI (which is deferred). -- Pros: Clean output, no extra rendering complexity. Cons: Does not match DESIGN.md's aspirational output.
- **Option 2**: Add a simple ASCII progress bar in the list output for stories with progress. Something like `[===---] 60%`. -- Pros: Matches DESIGN.md, more visual. Cons: Makes list lines longer, may not render well in narrow terminals.

### 6. `story show` for Type Rendering Differs From DESIGN.md Format
- **Severity**: Useful
- **Description**: DESIGN.md shows `story show` output as `type: epic` with `progress: 4/5 children done (80%)`. The actual implementation correctly shows `type: epic` but for untyped stories shows `type: -` (see Finding 1). The progress line matches the design: `progress: 2/3 children done (66%)`. There's also a minor code path where `children_total == 0` would output `progress: 0/0 children done (0%)` -- but this is unreachable since `compute_progress` returns `None` when children is empty.
- **Location**: `src/output.rs:381-389`
- **Option 1 (Recommended)**: Remove the dead `children_total == 0` branch in `render_story` since it's unreachable. -- Pros: Cleaner code, no dead branches. Cons: Defensive code removal; if `compute_progress` behavior changes, this guard disappears.
- **Option 2**: Keep it as defensive coding. -- Pros: Safety net if logic changes. Cons: Dead code.

## Design Alignment

**MINOR DRIFT**

The implementation aligns well with DESIGN.md overall. The deviations are:

1. **Default type display**: DESIGN.md says `None` should display as the default type from `types.toml`. Implementation shows `"-"` for `story show` and omits badge in list view. This is arguably a simpler and more honest approach, but differs from the spec.

2. **Output format**: List output uses `(done/total)` instead of the ASCII progress bar with percentage shown in DESIGN.md examples. Single-story output matches the design.

3. **Error message wording**: DESIGN.md specifies `type \`{slug}\` is not defined. Available types: ...` but implementation uses `unknown type \`{slug}\`. Available types: ...`. Minor wording difference, no behavioral impact.

All architectural decisions (event model, fold logic, types.toml lifecycle, epic sugar over primitives, progress rollup, parent skip, doctor integration, MCP schema updates, export/import) match DESIGN.md precisely.

## Strengths

- **Pattern consistency**: The implementation rigorously follows established patterns. `TypeDef` mirrors `StateDef`, `StoryTypeSet` mirrors `StoryPrioritySet`, `parse_type` follows `parse_phase`, `add_type`/`remove_type` follow `add_state`/`remove_state`. This makes the code predictable and maintainable.

- **Backward compatibility**: `#[serde(default)]` on `story_type`, `types` in `ProjectExport`, and `story_type` in `ImportStory` ensure old data deserializes cleanly. No migrations required.

- **Epic as sugar**: The `EpicAction` handlers delegate to existing primitives (create + type-set, relate parent-of, list filter, show). No parallel code paths, no duplicated logic.

- **Progress rollup**: Computed on read in `build_story_views` with a pre-computed `progress_map`, keeping the rendering layer thin. Direct children only, as specified.

- **Test coverage**: Integration tests cover the key scenarios (import with types, export/import round-trip, doctor flags unknown types, doctor does not flag known types, next skips parents). CLI parser tests are comprehensive with error cases.

- **Clean separation**: Storage layer handles config CRUD, domain handles events and computation, CLI handles parsing, app handles orchestration. No layer violations.

- **Validation at write time**: Type validation happens at write time (new, set, epic create) as specified, not during fold/replay. Doctor catches drift for out-of-band modifications.
