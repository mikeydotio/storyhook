# Implementation Plan: Fix Cycle 3 (Final)

**Fix cycle**: 3 of 3
**Items**: FIX-1 (`--json` patch missing `story_type`), FIX-2 (reserved slug "none" case sensitivity)

---

## Requirements

| ID | Requirement | Type | Priority |
|----|-------------|------|----------|
| R1 | `story set <id> --json '{"story_type":"..."}'` must work, mirroring the `--type` flag behavior | functional | high |
| R2 | The JSON patch error message must list `story_type` as a valid field | functional | high |
| R3 | Unknown `story_type` values in JSON patch must be rejected with a validation error | functional | high |
| R4 | `story type add None` and `story type add NONE` (any case variant) must be rejected as reserved | functional | medium |
| R5 | Existing lowercase `none` rejection must continue to work | functional | medium |

---

## Task Waves

### Wave 1 (parallel -- no dependencies)

#### T1.1: Add `story_type` arm to JSON patch dispatch table

- **Requirement(s)**: R1, R2, R3
- **Acceptance criteria**:
  - [ ] `src/app.rs` dispatch table (line ~1884-1948): new `"story_type"` match arm added between the `"blocked"` arm and the `other =>` fallback
  - [ ] The new arm validates the value is a string, loads the type map via `storage::load_type_map(root)?`, rejects unknown types with `AppError::Validation` containing the unknown slug and available types (mirrors the `--type` flag handler at lines 1863-1875)
  - [ ] The new arm pushes `StoryEvent::StoryTypeSet { at: now.clone(), story_type: v.to_string() }` and a change string `format!("type -> {v}")`
  - [ ] The error message at line 1948 updated from `"Valid fields: title, state, priority, assignee, labels, blocked"` to `"Valid fields: title, state, priority, assignee, labels, blocked, story_type"`
  - [ ] New integration test in `tests/story_types.rs`: `json_patch_sets_story_type` -- creates a project, adds type "bug", creates a story, runs `story set SH-1 --json '{"story_type":"bug"}'`, asserts success, then `story show SH-1` output contains `type: bug`
  - [ ] New integration test in `tests/story_types.rs`: `json_patch_rejects_unknown_story_type` -- runs `story set SH-1 --json '{"story_type":"nonexistent"}'`, asserts failure with output containing `"unknown type"` 
  - [ ] `cargo test --test story_types` passes with 0 failures
  - [ ] `cargo clippy -- -D warnings` reports 0 errors in `src/app.rs`
- **Expected files**: `src/app.rs`, `tests/story_types.rs`
- **Estimated scope**: small

#### T1.2: Make reserved slug "none" check case-insensitive

- **Requirement(s)**: R4, R5
- **Acceptance criteria**:
  - [ ] `src/storage.rs` line 419: `slug == "none"` changed to `slug.eq_ignore_ascii_case("none")`
  - [ ] No other lines in `src/storage.rs` are modified (the "default" check at line 424 is already correct)
  - [ ] Existing test `type_add_none_slug_rejected` in `tests/story_types.rs` still passes (lowercase "none")
  - [ ] New integration test in `tests/story_types.rs`: `type_add_none_case_insensitive` -- asserts that `story type add None` and `story type add NONE` both fail with output containing `"reserved"`
  - [ ] `cargo test --test story_types` passes with 0 failures
  - [ ] `cargo clippy -- -D warnings` reports 0 errors in `src/storage.rs`
- **Expected files**: `src/storage.rs`, `tests/story_types.rs`
- **Estimated scope**: small

---

## Requirement Traceability

| Requirement | Tasks | Coverage |
|-------------|-------|----------|
| R1: JSON patch `story_type` works | T1.1 | full |
| R2: Error message lists `story_type` | T1.1 | full |
| R3: Unknown types rejected in JSON patch | T1.1 | full |
| R4: Case-insensitive "none" rejection | T1.2 | full |
| R5: Existing lowercase "none" still rejected | T1.2 | full |

---

## Test Strategy

Both fixes add integration tests to the existing `tests/story_types.rs` file, following the established pattern (tempdir, `story init`, `story type add`, then assert). The final validation is:

```
cargo test --test story_types
cargo clippy -- -D warnings
cargo build --release
```

---

## Scope Boundaries

**IN scope**: The two triage items only -- `story_type` JSON patch arm and case-insensitive "none" slug check.

**OUT of scope**: Any other JSON patch fields, refactoring the dispatch table into a helper, adding `story_type` to other code paths (MCP already handles it separately), changing the "default" slug check (already correct).

---

## Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| `load_type_map` call in JSON arm duplicates call if `--type` flag also set | Double filesystem read, no correctness issue | Acceptable; both paths are idempotent reads. Refactoring is out of scope. |
| New tests import `predicate` or helpers not yet in scope | Compilation failure | Mirror exact imports and `story()` helper from existing tests in the same file |

---

## Deviation Log

| Task | Planned | Actual | Impact | Decision |
|------|---------|--------|--------|----------|
| (none yet) | | | | |

---

## Resumption State

- **Completed**: Plan created
- **Next**: Execute T1.1 and T1.2 in parallel (single wave, no dependencies)
- **Blockers**: None
