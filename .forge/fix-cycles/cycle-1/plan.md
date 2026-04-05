# Implementation Plan (Fix Cycle 1)

**Version**: v0.12.0
**Date**: 2026-04-04
**Scope**: 3 small findings from triage report -- all bug fixes, no new features

## Requirements

| ID | Requirement | Type | Priority |
|----|-------------|------|----------|
| R1 | MCP `storyhook_update_story` tool description must list `story_type` in the priority order field list | functional | high |
| R2 | `remove_type` must guard against removing the last remaining type to prevent `default_type()` hard error | functional | high |
| R3 | Dead `else` branch in progress rendering (`output.rs` lines 387-389) must be removed | non-functional (dead code) | medium |

## Task Waves

### Wave 1 (all independent -- no shared files, no dependencies)

#### T1.1: Update MCP tool description to include `story_type` in priority order
- **Requirement(s)**: R1
- **Description**: In `src/mcp.rs` line 185, the `storyhook_update_story` description string says "Processes one update field per call in priority order (state > priority > labels > assignee > awaiting)." The dispatch code (lines 540-599) actually processes `story_type` after `awaiting`. Update the parenthetical to: `(state > priority > labels > assignee > awaiting > story_type)`.
- **Acceptance criteria**:
  - [ ] The description string at `src/mcp.rs` for `storyhook_update_story` contains the text `story_type` in the priority order list
  - [ ] The priority order in the description matches the actual dispatch order: `state > priority > labels > assignee > awaiting > story_type`
  - [ ] `cargo build` completes with no errors
  - [ ] `cargo test --test mcp_server` passes (existing MCP integration tests still green)
- **Expected files**: `src/mcp.rs` (line 185, single string edit)
- **Estimated scope**: small

#### T1.2: Add last-type guard to `remove_type`
- **Requirement(s)**: R2
- **Description**: In `src/storage.rs`, the `remove_type` function (lines 441-462) does not check whether it is about to remove the last type. If it does, the `types.toml` becomes empty and `default_type()` (line 464) panics with a hard error. Add a guard after loading types: if `types.len() == 1` and the slug matches, return an `AppError::Validation` with message "cannot remove the last type".
- **Acceptance criteria**:
  - [ ] Calling `remove_type` when only one type exists returns `Err` containing the string "cannot remove the last type"
  - [ ] Calling `remove_type` when two or more types exist still succeeds for an unused, non-last type
  - [ ] A new unit test `remove_type_rejects_last_type` exists in `src/storage.rs` `mod tests` that: (a) sets up a project with exactly one type, (b) calls `remove_type`, (c) asserts the error message
  - [ ] A new integration test `type_remove_last_rejected` exists in `tests/story_types.rs` that: (a) inits a project, (b) removes types until one remains, (c) attempts to remove the last one via CLI, (d) asserts failure with the error message
  - [ ] `cargo test --test story_types` passes
  - [ ] `cargo test -p storyhook --lib storage::tests` passes (or equivalent unit test filter)
- **Expected files**: `src/storage.rs` (guard + unit test), `tests/story_types.rs` (integration test)
- **Estimated scope**: small

#### T1.3: Remove dead `else` branch in progress rendering
- **Requirement(s)**: R3
- **Description**: In `src/output.rs` lines 380-389, the `if let Some(ref progress) = view.progress` block has an `else` branch for `children_total == 0` that prints "0/0 children done (0%)". This branch is unreachable because `compute_progress` in `src/domain.rs` (lines 538-540) returns `None` when there are no children -- so if `progress` is `Some`, `children_total` is always > 0. Remove the `else` branch (lines 387-389) and simplify: remove the `if progress.children_total > 0` condition, keeping only the body since it is always true when progress is `Some`.
- **Acceptance criteria**:
  - [ ] `src/output.rs` no longer contains the string `0/0 children done`
  - [ ] `src/output.rs` no longer has the `if progress.children_total > 0` conditional -- the body runs unconditionally within the `if let Some(ref progress)` block
  - [ ] `cargo build` completes with no errors and no new warnings
  - [ ] `cargo test --test story_types` passes (the `show_displays_progress_in_human_output` and `epic_show_displays_progress` tests exercise this rendering path)
  - [ ] `cargo test` (full suite) passes with no regressions
- **Expected files**: `src/output.rs` (lines 380-389, remove ~4 lines)
- **Estimated scope**: small

## Requirement Traceability

| Requirement | Tasks | Coverage |
|-------------|-------|----------|
| R1: MCP description missing story_type | T1.1 | full |
| R2: remove_type last-type guard | T1.2 | full |
| R3: Dead code in progress rendering | T1.3 | full |

## Test Strategy

All three fixes are verified by:
1. **Existing tests**: The existing test suites (`story_types`, `mcp_server`) already exercise the happy paths around these areas. Any regression from the fixes will surface.
2. **New tests for T1.2 only**: FIX 2 introduces new behavior (the last-type guard), so it requires one new unit test and one new integration test. FIX 1 and FIX 3 are corrections to existing behavior already covered by tests.
3. **Full suite gate**: After all three fixes, `cargo test` must pass with zero failures and no new warnings.

## Risk Register

| Risk | Impact | Mitigation |
|------|--------|------------|
| T1.3 simplification introduces division-by-zero if `compute_progress` invariant changes in the future | Would panic at runtime when rendering progress for a parent with zero children | Low risk -- the invariant is structural (None returned before total is computed). Add a comment in `output.rs` noting the invariant: "progress is Some only when children_total > 0 (see compute_progress)" |
| T1.2 guard message wording does not match CLI error rendering convention | User-facing error looks inconsistent | Check existing `AppError::Validation` messages for tone/casing before writing the new one |

## Scope Boundaries

**IN scope**:
- The 3 specific fixes described above
- New tests only for T1.2 (new behavior)
- No refactoring beyond the minimal changes

**OUT of scope**:
- Refactoring the MCP dispatch pattern
- Adding comprehensive MCP description validation tests
- Restructuring `compute_progress` return type
- Any other triage findings not listed as FIX

## Deviation Log

| Task | Planned | Actual | Impact | Decision |
|------|---------|--------|--------|----------|
| (none yet) | | | | |

## Resumption State

- **Completed tasks**: none
- **In-progress task**: none
- **Next tasks**: T1.1, T1.2, T1.3 (all ready to start in parallel)
- **Blockers**: none
- **Context**: All three tasks touch different files with no shared interfaces. They can be executed in any order or simultaneously.
