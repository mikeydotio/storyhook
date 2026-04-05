# Triage Report

## Summary
- Total findings: 2 (review: 2, validate: 0 outstanding)
- FIX: 2
- ESCALATE: 0
- Yolo mode: false
- Fix cycle: 2 / 3

Note: The VALIDATE-REPORT.md contained 4 Useful findings, all of which were resolved in-session during the validate step (tests added, unused import removed). No outstanding validation findings require triage.

## FIX Items

### 1. `--json` Patch Missing `story_type` Field — FIX
- **Source**: REVIEW-REPORT.md
- **Severity**: Important
- **Chosen Solution**: Option 1 — Add `story_type` arm to the JSON patch dispatch table in `src/app.rs:1884-1948`
- **Rationale**: Single obvious correct solution. The exact pattern already exists for the `--type` flag handler. The fix is additive (~10 lines), follows existing patterns, and cannot break other match arms. Leaving the inconsistency would confuse programmatic callers using `--json`.
- **Scope**: `src/app.rs` (dispatch table + error message) + new integration test
- **Acceptance**: `story set <id> --json '{"story_type":"epic"}'` succeeds; unknown types rejected; error message lists `story_type` as valid field

### 2. Reserved Slug "none" Check Case Sensitivity — FIX
- **Source**: REVIEW-REPORT.md
- **Severity**: Useful
- **Chosen Solution**: Option 1 — Change `slug == "none"` to `slug.eq_ignore_ascii_case("none")` at `src/storage.rs:419`
- **Rationale**: Trivial 1-line change that makes reserved slug handling consistent ("default" already uses case-insensitive check). Prevents unlikely but real collision with `--type none` filter semantics. Zero risk.
- **Scope**: `src/storage.rs` line 419 + extend existing test
- **Acceptance**: `story type add None` and `story type add NONE` both rejected; existing `none` test still passes

## ESCALATE Items

None.
