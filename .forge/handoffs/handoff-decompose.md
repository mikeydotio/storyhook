# Handoff: Decompose Complete (Fix Cycle 3)

## Timestamp
2026-04-05T15:21:00Z

## Artifacts Produced
- `.forge/plan-mapping.json` (2 stories mapped to tasks)

## Key Decisions
- Created 3 stories: SH-28 (parent), SH-29 (T1.1 JSON patch), SH-30 (T1.2 case-insensitive none)
- Both child stories in wave 1 (parallel, no inter-dependencies)
- DAG validated — no cycles detected
- Design sections embedded in plan-mapping.json for execution context

## Context for Next Step

### SH-29 (T1.1): Add `story_type` to JSON patch dispatch
- File: `src/app.rs`, lines 1884-1948
- Add `"story_type"` match arm between `"blocked"` and `other =>`
- Mirror `--type` flag handler (lines 1863-1875): load type map, validate slug, emit `StoryTypeSet`, push change string
- Update error message at line 1948: add `story_type` to valid fields list
- 3 red tests already written: `json_patch_sets_story_type`, `json_patch_rejects_invalid_story_type`, `json_patch_unknown_field_error_lists_story_type`

### SH-30 (T1.2): Case-insensitive "none" slug check
- File: `src/storage.rs`, line 419
- Change `slug == "none"` to `slug.eq_ignore_ascii_case("none")`
- 3 red tests already written: `type_add_none_titlecase_rejected`, `type_add_none_uppercase_rejected`, `type_add_none_mixedcase_rejected`

## Pipeline State
- Fix cycle: 3 / max 3 (final cycle)
- Yolo mode: false
- All 27 original stories complete
- 6 red tests awaiting fixes
- 2 new stories (SH-29, SH-30) in todo state

## Open Questions
None — both fixes are fully specified with pre-written tests.
