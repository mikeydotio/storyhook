# Validation Report

## Test Suite Results
- Total: 648 | Pass: 648 | Fail: 0 | Skip: 0
- Run command: `cargo test`
- Duration: ~5.4s
- Baseline (before this step): 619 tests
- Tests added: 29

## Findings

### 1. No integration tests for `story type list/add/remove` CLI commands
- **Severity**: Important
- **Description**: The `story type list`, `story type add`, and `story type remove` commands had unit tests in `storage.rs` and CLI parsing tests in `cli.rs`, but no end-to-end integration tests verifying the commands work through the binary. Edge cases like duplicate slugs, reserved "none" slug, removal of in-use types, and removal of nonexistent types had no integration coverage.
- **Option 1 (Implemented)**: Write integration tests in `tests/story_types.rs` covering list, add (with description), add duplicate, add "none" slug, remove unused, remove in-use, remove nonexistent. — Pros: Catches CLI→app→storage integration bugs. Cons: None (tests are fast, ~0.3s for the whole file).
- **Option 2**: Rely on unit tests alone. — Pros: Fewer test files. Cons: Misses integration-level regressions (e.g., argument parsing changes silently breaking commands).

### 2. No integration tests for `--type` flag on new/set/list
- **Severity**: Important
- **Description**: The `--type` flag on `story new`, `story set`, and `story list` had no integration tests. Type validation errors (unknown type rejected), type display in `story show`, and `--type none` filter for untyped stories were untested at the binary level.
- **Option 1 (Implemented)**: Write integration tests covering `story new --type bug`, `story new --type nonexistent` (rejected), `story set --type epic`, `story set --type nonexistent` (rejected), `story list --type bug` (filter), `story list --type none` (untyped filter), and combined `--type` + `--priority` filtering. — Pros: Full coverage of the flag through all three commands. Cons: None.
- **Option 2**: Rely on unit tests for parsing and manual testing for integration. — Pros: Less test code. Cons: No automated regression protection for the flag-to-handler pathway.

### 3. No integration tests for `story epic` subcommands
- **Severity**: Important
- **Description**: The `story epic create/add/list/show` commands had no integration tests. Epic-specific behaviors like two-event pattern (create + type-set), parent-child relationship creation via `epic add`, filtered listing to only epics, progress display in `epic show`, and the edge case where the "epic" type doesn't exist were all untested at the binary level.
- **Option 1 (Implemented)**: Write integration tests in `tests/story_types.rs` for epic create (verifying type=epic is set), epic add (verifying parent-of/child-of edges), epic list (only epics with progress), epic show (progress display), empty epic list, and epic create with missing type. — Pros: Comprehensive epic sugar coverage. Cons: None.
- **Option 2**: Test only via the existing primitives (story new, relate, list --type). — Pros: Less duplication. Cons: Epic sugar commands have their own validation logic (e.g., checking "epic" type exists) that wouldn't be tested.

### 4. No integration tests for MCP tools with `story_type` parameter
- **Severity**: Important
- **Description**: The MCP tools `storyhook_create_story`, `storyhook_update_story`, and `storyhook_list_stories` all gained a `story_type` parameter, but there were no MCP integration tests verifying the parameter flows correctly through `build_invocation` to the handlers.
- **Option 1 (Implemented)**: Write MCP integration tests for create with type, update type, and list with type filter. — Pros: Validates the MCP→CLI invocation mapping and JSON response format. Cons: None.
- **Option 2**: Rely on the CLI integration tests and the `build_invocation` mapping being simple. — Pros: Fewer tests. Cons: MCP parameter naming/mapping errors would go undetected.

### 5. No integration test for progress rollup in human output
- **Severity**: Useful
- **Description**: Progress rollup was tested in JSON output (`relationships.rs` tests check `children_total`/`children_done`), but not in human-readable output. The `progress: X/Y children done (Z%)` line in `story show` had no integration test.
- **Option 1 (Implemented)**: Write tests verifying the human-readable progress line in `story show`, including before/after closing children. — Pros: Catches formatting regressions. Cons: None.
- **Option 2**: Rely on JSON output tests and visual inspection. — Pros: Less test surface. Cons: Human output format could regress silently.

### 6. No E2E lifecycle test for the full epic workflow
- **Severity**: Useful
- **Description**: While individual features were tested, there was no single test exercising the complete epic lifecycle: create epic, add children, verify progress, complete children, verify 100% progress, check epic list, check story next skipping.
- **Option 1 (Implemented)**: Write a comprehensive E2E test (`full_epic_lifecycle`) exercising the complete workflow in sequence. — Pros: Validates features work together, catches interaction bugs. Cons: Slower test (~0.1s) but still fast.
- **Option 2**: Rely on individual feature tests. — Pros: Easier to debug failures. Cons: Misses integration-level issues between features.

### 7. `story_type` field omitted (not null) in JSON for untyped stories
- **Severity**: Useful
- **Description**: When a story has no type, the JSON output omits the `story_type` field entirely due to `skip_serializing_if = "Option::is_none"`. This is consistent with other optional fields (assignee, closed_at) and is the correct behavior per the design (backward-compatible with older consumers). However, API consumers need to handle both "field absent" and "field present" cases.
- **Option 1 (Recommended)**: Keep current behavior. Document in API docs that `story_type` is absent for untyped stories. The test `json_output_omits_type_for_untyped_story` verifies this behavior. — Pros: Backward compatible, consistent with other fields. Cons: Consumers must check for key existence.
- **Option 2**: Always serialize `story_type` as null when None. — Pros: Uniform shape. Cons: Breaking change for existing JSON consumers who don't expect the field.

### 8. `story summary` and `story context` do not include type breakdown
- **Severity**: Useful
- **Description**: The IDEA.md lists "`story summary` and `story context` surface epic/type information" as a key requirement. The PLAN.md scope boundaries explicitly defer this: "story summary/story context type breakdown (DESIGN.md open question)." No type breakdown is rendered in summary or context output.
- **Option 1 (Recommended)**: Accept as deferred scope. The current behavior is consistent with the PLAN.md scope boundaries. Track as a future enhancement. — Pros: Clean scope boundary, no risk. Cons: Feature gap vs IDEA.md.
- **Option 2**: Implement type breakdown in summary/context. — Pros: Fulfills IDEA.md requirement. Cons: Scope expansion, needs separate story.

## Requirement Coverage

| Requirement | Tested? | Test Location | Notes |
|------------|---------|---------------|-------|
| Configurable story type system via types.toml | YES | storage.rs unit tests, tests/story_types.rs (type_list, type_add, type_remove) | Full CRUD coverage |
| Default types: story, epic, bug, chore, task | YES | storage.rs::load_types_returns_default_types, tests/story_types.rs::type_list_shows_default_types | |
| StoryTypeSet event in event-sourcing model | YES | domain.rs::fold_story_tracks_story_type, fold_story_story_type_can_be_changed | |
| story type add/remove/list CLI commands | YES | tests/story_types.rs (7 tests) | Including error paths |
| story new --type epic "Title" | YES | tests/story_types.rs::new_with_type_creates_typed_story | |
| story set --type epic | YES | tests/story_types.rs::set_type_changes_story_type | |
| story epic list | YES | tests/story_types.rs::epic_list_shows_only_epics_with_progress, epic_list_empty_when_no_epics | |
| story epic show | YES | tests/story_types.rs::epic_show_displays_progress | |
| story epic create | YES | tests/story_types.rs::epic_create_sets_type_to_epic | |
| story epic add | YES | tests/story_types.rs::epic_add_creates_parent_child_relationship | |
| Universal progress rollup | YES | domain.rs unit tests (4), relationships.rs integration (2), tests/story_types.rs (2) | |
| story next skips parents | YES | tests/story_next.rs::next_skips_parent_stories, next_returns_no_ready_when_only_parents_are_ready | |
| story list --type filter | YES | tests/story_types.rs (3 tests including --type none and combined filters) | |
| MCP tools updated with type field | YES | tests/story_types.rs::mcp_create/update/list | |
| story summary/context type info | NO | — | Explicitly deferred in PLAN.md scope boundaries |
| story doctor type integrity check | YES | tests/doctor.rs::doctor_flags_unknown_story_type, doctor_does_not_flag_known_story_type | |
| Migration (auto-create types.toml) | YES | storage.rs::ensure_types_file_creates_defaults, load_types_auto_creates_if_missing, init_project_creates_types_file | |
| Backward compat (old snapshots) | YES | domain.rs::fold_story_story_type_defaults_to_none, implicit in all tests creating stories without --type | |
| Type validation at write time | YES | tests/story_types.rs::new_with_unknown_type_rejected, set_unknown_type_rejected | |
| Export/import types config | YES | tests/story_export.rs::export_and_import_roundtrip_with_types | |
| ImportStory story_type field | YES | tests/story_import.rs::import_with_story_type | |
| Default type resolution at display | YES | tests/story_types.rs::untyped_story_shows_dash_for_type | Displays "-" not default type name |
| Epic create two-event pattern | YES | tests/story_types.rs::epic_create_sets_type_to_epic (verifies type: epic in show) | |
| Progress in JSON output | YES | tests/relationships.rs::parent_story_shows_progress_rollup_in_json | |
| Progress in human output | YES | tests/story_types.rs::show_displays_progress_in_human_output | |
| Type badge in list output | YES | tests/story_types.rs::list_shows_type_badge_in_human_output | |
| Reserved slug "none" rejected | YES | storage.rs unit test, tests/story_types.rs::type_add_none_slug_rejected | |
| Remove in-use type rejected | YES | storage.rs unit test, tests/story_types.rs::type_remove_in_use_rejected | |
| Help text updated | YES | cli.rs tests verify parse_invocation routes type/epic commands | Not explicitly tested for HELP_TEXT string content |

## Tests Written This Step

- `tests/story_types.rs` (29 tests): Comprehensive integration test file covering all story types and epics features that lacked E2E integration tests:
  - **type CRUD** (7 tests): list default types, add with description, add duplicate rejected, add "none" rejected, remove unused, remove in-use rejected, remove nonexistent rejected
  - **--type flag** (7 tests): new with type, new with unknown type rejected, set type, set unknown type rejected, untyped story display, list filter, list --type none, combined filter
  - **epic subcommands** (5 tests): create sets type to epic, add creates relationship, list shows only epics with progress, show displays progress, empty list, create rejects when epic type not defined
  - **progress rendering** (2 tests): human output progress line, type badge in list
  - **JSON output** (2 tests): story_type field present when set, omitted when untyped
  - **MCP tools** (3 tests): create with type, update type, list with type filter
  - **E2E lifecycle** (1 test): full epic create→add children→progress→complete→verify workflow

## Strengths

1. **Thorough unit test coverage**: The domain.rs, storage.rs, and cli.rs modules all have comprehensive unit tests covering their new functionality. The existing test patterns (fold tests, storage CRUD tests, parser tests) were correctly extended for types/epics.

2. **Good test patterns**: The project uses `assert_cmd` + `tempdir` for integration tests, which provides clean isolated environments. Tests are fast (~5.4s total for 648 tests) and deterministic.

3. **Layered testing**: Unit tests validate core logic (fold, compute_progress, has_children), integration tests validate CLI commands, and the doctor tests validate data integrity checking. This provides good defense-in-depth.

4. **Error path coverage**: Both unit tests and integration tests cover error cases (unknown types, duplicate types, reserved slugs, in-use type removal, self-referencing epic add, cycle detection).

5. **Backward compatibility**: The `#[serde(default)]` annotation on `story_type` and the fold test verifying `story_type: None` for old events provides strong backward compat guarantees.

6. **Progress rollup computed correctly**: The `compute_progress` function correctly handles direct-children-only semantics, includes archived children via `build_story_views` loading both open and archived, and returns `None` for leaf stories.
