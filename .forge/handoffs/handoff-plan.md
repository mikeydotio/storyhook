# Handoff: Plan Complete (Fix Cycle 3)

## Timestamp
2026-04-05T12:00:00Z

## Artifacts Produced
- `.forge/PLAN.md` (fix cycle 3 — 2 tasks, 1 wave)
- `tests/story_types.rs` (6 new red tests added by QA agent)

## Key Decisions
- Plan approved by user — 2 FIX items, single parallel wave
- QA pre-wrote 6 integration tests (3 for each fix) — all confirmed failing against current code
- Both fixes are independent, no dependencies

## Context for Next Step

### T1.1: Add `story_type` to JSON patch dispatch
- File: `src/app.rs`, lines 1884-1948
- Add `"story_type"` match arm between `"blocked"` and `other =>`
- Mirror the `--type` flag handler (lines 1863-1875): load type map, validate slug, emit `StoryTypeSet`, push change string
- Update error message at line 1948: add `story_type` to valid fields list
- Tests: `json_patch_sets_story_type`, `json_patch_rejects_invalid_story_type`, `json_patch_unknown_field_error_lists_story_type`

### T1.2: Case-insensitive "none" slug check
- File: `src/storage.rs`, line 419
- Change `slug == "none"` to `slug.eq_ignore_ascii_case("none")`
- Tests: `type_add_none_titlecase_rejected`, `type_add_none_uppercase_rejected`, `type_add_none_mixedcase_rejected`

## Pipeline State
- Fix cycle: 3 / max 3 (final cycle)
- Yolo mode: false
- All 27 original stories complete
- 6 new red tests awaiting fixes

## Open Questions
None — both fixes are fully specified.
