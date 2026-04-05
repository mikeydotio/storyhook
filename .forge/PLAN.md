# Implementation Plan: ESCALATE Fix Cycle (SH-12, SH-13, SH-15)

**Fix cycle**: 1
**Stories**: SH-12, SH-13, SH-15

---

## Task Waves

### Wave 1 (parallel — no dependencies)

#### T1.1: Display "Default" for untyped stories + reserve slug (SH-12)

- **Acceptance criteria**:
  - [ ] `src/output.rs:342`: fallback changed from `"-"` to `"Default"` — `story show` prints `type: Default` for untyped stories
  - [ ] `src/output.rs:256-259`: list view renders `[Default]` badge for `story_type == None` instead of empty string
  - [ ] `src/storage.rs` `add_type`: rejects slug "default" (case-insensitive) in addition to "none" — returns `AppError::Validation` with message `"type slug \`default\` is reserved"`
  - [ ] `tests/story_types.rs`: `untyped_story_shows_dash_for_type` updated to assert `type: Default`
  - [ ] `tests/story_import.rs`: `import_with_story_type` updated to assert `type: Default` for untyped story
  - [ ] New test: `type_add_rejects_reserved_default_slug` — `story type add default` fails with reserved slug error
  - [ ] No `use` of storage or types.toml added to output.rs
  - [ ] `cargo test --test story_types` passes
  - [ ] `cargo test --test story_import -- import_with_story_type` passes
- **Files**: `src/output.rs`, `src/storage.rs`, `tests/story_types.rs`, `tests/story_import.rs`

#### T1.2: Add import validation for story_type against types.toml (SH-13)

- **Acceptance criteria**:
  - [ ] `src/app.rs`: before the import loop (between line 725 and 730), `load_type_map(root)?` is called
  - [ ] All `import_story.story_type` values are collected and validated before the loop — any invalid types cause early `Err(AppError::Validation(...))`
  - [ ] Error message lists ALL invalid types found (not just the first), e.g. `"unknown types: foo, bar. Available types: story, epic, bug, chore, task"`
  - [ ] Zero stories are created when validation fails (all-or-nothing)
  - [ ] New test `import_rejects_invalid_story_type`: imports JSON with invalid type, asserts failure with error containing the type name
  - [ ] New test `import_atomicity_on_type_error`: after failed import, `story list` returns no stories
  - [ ] New test `import_accepts_valid_story_type`: imports JSON with valid type, asserts success
  - [ ] `cargo test --test story_import` passes
- **Files**: `src/app.rs`, `tests/story_import.rs`

---

### Wave 2 (depends on Wave 1)

#### T2.1: Add type breakdown to SummaryView and Summary/Report handlers (SH-15)

- **Depends on**: T1.1 ("Default" string convention)
- **Acceptance criteria**:
  - [ ] `SummaryView` in `src/output.rs` gains `pub by_type: Vec<(String, usize)>`
  - [ ] `Invocation::Summary` handler in `src/app.rs`: counts types using "Default" for `story_type == None`, passes as `by_type`
  - [ ] Both `Report { html: false }` and `Report { html: true }` handlers: same type counting, passed to `SummaryView`
  - [ ] `render_summary` in `src/output.rs`: renders "by type:" section after "by priority:" section
  - [ ] New test `summary_shows_type_breakdown`: creates typed + untyped stories, asserts `"by type:"` with correct counts including `"Default"`
  - [ ] `cargo test --test story_summary` passes
- **Files**: `src/output.rs`, `src/app.rs`, `tests/story_summary.rs`

#### T2.2: Add type breakdown to Context handler (SH-15)

- **Depends on**: T1.1 ("Default" string convention)
- **Acceptance criteria**:
  - [ ] `Invocation::Context` JSON branch: `by_type` BTreeMap included in JSON output
  - [ ] `Invocation::Context` plain text branch: "## Type Distribution" section added after "## State Distribution"
  - [ ] New test `context_shows_type_distribution`: plain text output contains "Type Distribution" and type counts
  - [ ] New test `context_json_includes_type_distribution`: JSON output contains `"by_type"` with correct counts
  - [ ] `cargo test --test story_context` passes
- **Files**: `src/app.rs`, `tests/story_context.rs`

#### T2.3: Add type breakdown to HTML report (SH-15)

- **Depends on**: T2.1 (`by_type` field on `SummaryView`)
- **Acceptance criteria**:
  - [ ] `render_html_report` in `src/output.rs`: new "Type Breakdown" section after Priority Breakdown
  - [ ] New test `report_html_shows_type_breakdown`: HTML output contains "Type Breakdown" and type names
  - [ ] `cargo test --test story_report` passes
- **Files**: `src/output.rs`, `tests/story_report.rs`

---

### Wave 3 (final validation)

#### T3.1: Full test suite validation

- **Depends on**: all previous
- **Acceptance criteria**:
  - [ ] `cargo test` — 0 failures
  - [ ] `cargo clippy -- -D warnings` — 0 errors
  - [ ] `cargo build --release` — succeeds

---

## Scope Boundaries

**IN scope**: SH-12 display change + "default" slug reservation, SH-13 import validation (all-or-nothing), SH-15 type breakdown in summary/context/HTML

**OUT of scope**: JSON serialization of story_type (remains null/omitted for untyped), refactoring duplicated summary logic, TUI changes, making "Default" configurable

## Risk Register

| Risk | Impact | Mitigation |
|------|--------|------------|
| "Default" slug collision | Users can't distinguish typed vs untyped | Reserve "default" slug (case-insensitive) in T1.1 |
| SH-13 one-at-a-time error reporting | Painful UX for bulk imports | Collect all invalid types before returning error |
| SummaryView struct change breaks compilation | Build failure in Wave 2 tasks | T2.1 updates all 3 construction sites in one task |
| 2 existing tests assert `type: -` | CI failure | T1.1 explicitly updates both tests |
