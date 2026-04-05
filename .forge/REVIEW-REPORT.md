# Review Report

## Summary

The Story Types & Epics implementation is mature and well-tested after the fix cycle. All 3 FIX items (MCP description, last-type guard, dead code branch) and all 4 ESCALATE items (SH-12 default display, SH-13 import validation, SH-14 progress format accepted, SH-15 type breakdown) from cycle 0 have been resolved. The codebase passes `cargo test` (0 failures), `cargo clippy -- -D warnings` (0 errors), and `cargo build --release` cleanly. Two new findings emerged during this review — one Important and one Useful.

## Findings

### 1. `--json` Patch in `story set` Does Not Recognize `story_type` Field
- **Severity**: Important
- **Description**: The `story set <id> --json '{"story_type":"epic"}'` command returns `unknown field 'story_type' in JSON. Valid fields: title, state, priority, assignee, labels, blocked`. The `--type` flag on `story set` works correctly, and the MCP `storyhook_update_story` also works via `SetFields`. But the `--json` patch path in the `SetFields` handler has a field dispatch table (lines 1884-1948) that does not include a `"story_type"` arm. This means programmatic callers using `--json` cannot set story type through that interface, creating an inconsistency between the `--type` flag and `--json` patch.
- **Location**: `src/app.rs:1884-1948` (the `for (key, value) in obj` loop in `SetFields` handler)
- **Option 1 (Recommended)**: Add a `"story_type"` arm to the JSON patch dispatch table that mirrors the `--type` flag behavior: validate against `load_type_map`, emit `StoryTypeSet`, push a change string. Also update the error message at line 1948 to list `story_type` as a valid field. -- Pros: Consistent behavior, small change, follows existing pattern. Cons: One more arm in the match.
- **Option 2**: Accept the inconsistency and document that `--json` does not support `story_type`. Users should use `--type` instead. -- Pros: Zero code change. Cons: Inconsistent API surface, confusing for users who discover `--json` accepts other fields but not type.

### 2. Reserved Slug "none" Check Is Case-Sensitive While "default" Is Case-Insensitive
- **Severity**: Useful
- **Description**: In `storage::add_type`, the reserved slug "none" is checked with exact match (`slug == "none"`) while "default" is checked case-insensitively (`slug.eq_ignore_ascii_case("default")`). This means `story type add None` or `story type add NONE` would succeed, creating a type slug that collides with the `--type none` filter semantics (which checks for exact lowercase "none" to mean "untyped stories"). In practice this is unlikely since type slugs are conventionally lowercase, but the inconsistency between the two reserved slug checks is a code smell.
- **Location**: `src/storage.rs:419` (the `slug == "none"` check) vs `src/storage.rs:424` (the `slug.eq_ignore_ascii_case("default")` check)
- **Option 1 (Recommended)**: Change the "none" check to also be case-insensitive: `slug.eq_ignore_ascii_case("none")`. -- Pros: Consistent with "default" handling, prevents "None"/"NONE" slug creation, 1-line change. Cons: Slightly stricter.
- **Option 2**: Keep the current behavior. The "none" slug is really about the CLI filter `--type none`, and users adding a slug "None" (capitalized) is an edge case not worth guarding. -- Pros: No change. Cons: Inconsistent reserved slug handling.

## Design Alignment

**ALIGNED**

All prior design drift items from cycle 0 have been resolved:

1. **Default type display**: Now shows "Default" instead of "-", matching the design intent that every story has a displayable type. The DESIGN.md wording about displaying "the default type from types.toml" was simplified to a hardcoded "Default" string, which is documented and accepted.

2. **Output format**: The compact `(done/total)` format for progress was accepted via ESCALATE SH-14, so the lack of ASCII progress bars is no longer drift.

3. **Import validation**: SH-13 resolved — import now validates all story_type values against types.toml before creating any stories (all-or-nothing).

4. **Type breakdown**: SH-15 resolved — summary, context (plain text and JSON), and HTML report all include type breakdown.

5. **MCP description**: Updated to list `story_type` in the one-field-per-call priority order.

6. **Last-type guard**: `remove_type` now rejects removal of the last type.

7. **Dead code**: The unreachable `children_total == 0` branch has been removed.

All architectural decisions (event model, fold logic, types.toml lifecycle, epic sugar over primitives, progress rollup, parent skip, doctor integration, MCP schema updates, export/import) match DESIGN.md precisely.

## Story Hygiene

All 27 stories (SH-1 through SH-27) are complete. The fix cycle stories (SH-22 through SH-27) have been reconciled and archived. No story-code drift detected — the implementation matches the story acceptance criteria.

## Strengths

- **Comprehensive test coverage**: 806 lines of type/epic-specific integration tests across 6 test files (story_types.rs, story_import.rs, story_export.rs, story_summary.rs, story_context.rs, story_report.rs), plus doctor tests for type integrity. The full epic lifecycle E2E test is particularly thorough.

- **Fix cycle quality**: All 3 FIX and 4 ESCALATE items from cycle 0 were resolved correctly. The "Default" display convention, import validation, type breakdown, and reserved slug handling are all clean.

- **Pattern consistency maintained**: Type counting uses the same `unwrap_or("Default")` pattern in all 4 summary-building paths (Summary, Report-text, Report-HTML, Context). The BTreeMap<String, usize> ensures deterministic ordering.

- **Backward compatibility preserved**: `#[serde(default)]` on `story_type` in StorySnapshot, `#[serde(default)]` on `types` in ProjectExport. Old data deserializes cleanly. types.toml auto-creates on first use.

- **Clean toolchain**: Zero clippy warnings, zero test failures, clean release build. The 25 pre-existing clippy warnings cleaned up in SH-27 improve long-term maintainability.

- **Import atomicity**: The all-or-nothing import validation (collecting all invalid types before failing) is a strong UX pattern that prevents partial imports.

- **Reserved slug protection**: Both "none" and "default" are protected in `add_type`, and the last-type removal guard prevents `default_type()` from erroring on an empty types list.
