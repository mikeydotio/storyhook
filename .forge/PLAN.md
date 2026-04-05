# Implementation Plan (Fix Cycle 1)

Scope: 3 FIX items from triage. All independent, single wave.

## Task Breakdown

### Wave 1 (all independent — no dependencies)

- [ ] Task 1.1: Add `story_type` to MCP update tool description
  - Acceptance: The `storyhook_update_story` tool description string includes `story_type` in the priority order list (state > priority > labels > assignee > awaiting > story_type)
  - Files: `src/mcp.rs` (line 185, description string only)

- [ ] Task 1.2: Guard against removing last type in `remove_type`
  - Acceptance: `remove_type` returns `AppError::Validation` with a message containing "last" when called with the only remaining type in types.toml. New unit test `remove_type_rejects_last_type` passes.
  - Files: `src/storage.rs` (guard clause in `remove_type` at line 441 + new test after line 1210)

- [ ] Task 1.3: Remove dead code branch in progress rendering
  - Acceptance: The `else` branch at output.rs:387-389 (printing "0/0 children done (0%)") is removed. The `if progress.children_total > 0` conditional is simplified since it's always true when progress is `Some`. All existing tests pass.
  - Files: `src/output.rs` (lines 380-389)

## Test Strategy

- **Task 1.1**: No test needed — documentation string change, no behavior
- **Task 1.2**: One new unit test `remove_type_rejects_last_type` in `src/storage.rs` — setup with single type, attempt removal, assert Validation error
- **Task 1.3**: No test needed — dead code removal. Invariant already proven by `compute_progress_returns_none_for_leaf` in `src/domain.rs`
- **Gate**: Full `cargo test` after all changes

## Resumption Points

After Wave 1, all fixes are complete. Single wave = single resumption boundary.

## Risk Register

- **Low**: All three changes are under 10 lines each, in separate files, with no shared dependencies. Risk of regression is minimal.
- **Note**: Task 1.2 introduces new behavior (rejection). The test covers the boundary case.
