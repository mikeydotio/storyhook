# Work Handoff — Triage (Cycle 2)

## Session Summary
- **Step**: triage (cycle 2)
- **Status**: Complete — 2 FIX, 0 ESCALATE

## Key Decisions

### FIX-1: `--json` Patch Missing `story_type`
- Add `story_type` arm to JSON dispatch table in `src/app.rs:1884-1948`
- Mirrors existing `--type` flag handler pattern
- Also update error message to list `story_type` as valid field
- Add integration test for `--json` path

### FIX-2: Reserved Slug "none" Case Sensitivity
- Change `slug == "none"` to `slug.eq_ignore_ascii_case("none")` at `src/storage.rs:419`
- 1-line change for consistency with "default" handling
- Extend test to cover `None`/`NONE` variants

## Context for Next Step

The orchestrator should:
1. Archive current cycle artifacts (TRIAGE.md, PLAN.md, plan-mapping.json) to `.forge/fix-cycles/cycle-2/`
2. Dispatch to `plan --orchestrated` with the 2 FIX items as input

Both fixes are localized and independent. They should not require a cycle-3.

## Pipeline State
- Fix cycle: 2 / max 3
- Yolo: false
- All 27 original stories complete
- 659 tests passing, 0 clippy warnings
- ESCALATE stories: none
