# Triage Report

## Summary
- Total findings: 14 (6 review + 8 validate)
- FIX: 3
- ESCALATE: 4
- RESOLVED (tests written): 6
- ACCEPTED (no action): 1
- Yolo mode: false
- Fix cycle: 0 / 3

## FIX Items

### MCP `storyhook_update_story` Tool Description Missing `story_type` — FIX
- **Source**: REVIEW-REPORT Finding #2
- **Severity**: Important
- **Chosen Solution**: Update the MCP tool description to explicitly mention that `story_type` is subject to the one-field-per-call limitation, and list it in the priority order.
- **Rationale**: Single obvious fix — documentation-only change, no behavior change, no risk. The one-field-per-call pattern is pre-existing architecture; the description just needs to enumerate `story_type` in the field list.

### `remove_type` Does Not Guard Against Removing Last Type — FIX
- **Source**: REVIEW-REPORT Finding #4
- **Severity**: Useful
- **Chosen Solution**: Add a guard in `remove_type` that returns an error when attempting to remove the last type in types.toml (e.g., "cannot remove the last type").
- **Rationale**: Single obvious fix — prevents a hard error from `default_type()` when the types list becomes empty. No user-facing behavior change (removing the last type is almost certainly unintentional). Low risk.

### Dead Code Branch in Progress Rendering — FIX
- **Source**: REVIEW-REPORT Finding #6
- **Severity**: Useful
- **Chosen Solution**: Remove the unreachable `children_total == 0` branch in `render_story` since `compute_progress` returns `None` when there are no children.
- **Rationale**: Single obvious fix — removes dead code, no behavior change. `compute_progress` already guarantees this branch is unreachable.

## ESCALATE Items

### Default Type Display — "-" vs Default Type Name — ESCALATE
- **Source**: REVIEW-REPORT Finding #1
- **Severity**: Important
- **Story**: SH-12
- **Description**: DESIGN.md specifies that `story_type: None` should render as the default type from types.toml (e.g., "type: story"). Implementation shows "type: -" in `story show` and omits the badge in list view.
- **Options**:
  1. **Accept current behavior** — Keep "-" display, document it. Pros: Simpler code, honest about stored data. Cons: Diverges from DESIGN.md.
  2. **Pass default type as fallback** — Load default_type in app.rs, thread through StoryView. Pros: Matches DESIGN.md. Cons: Adds coupling between output and storage layers.
- **Recommendation**: Option 1 — simpler, output.rs stays independent of storage
- **Rationale**: Changes user-facing display behavior. Multiple valid approaches with different trade-offs. User should decide the display philosophy.

### Import Validation — Validate `story_type` Against types.toml — ESCALATE
- **Source**: REVIEW-REPORT Finding #3
- **Severity**: Useful
- **Story**: SH-13
- **Description**: `story import` writes StoryTypeSet events without validating against types.toml. Other write paths (new, set) validate eagerly. Doctor catches drift afterward.
- **Options**:
  1. **Add validation pass** — Check each ImportStory.story_type before import loop. Pros: Consistent validation. Cons: Stricter import may break migration workflows.
  2. **Accept current behavior** — Keep import permissive, doctor catches drift. Pros: Flexible for migrations. Cons: Inconsistent validation.
- **Recommendation**: Option 2 — import is a bulk/migration tool where strictness may be counterproductive
- **Rationale**: Design decision about import strictness. Import intentionally serves different use cases (migration, bulk ops) than interactive commands.

### Progress Bar Format in List Output — ESCALATE
- **Source**: REVIEW-REPORT Finding #5
- **Severity**: Useful
- **Story**: SH-14
- **Description**: DESIGN.md shows ASCII progress bars in `story epic list` output. Implementation uses compact `(done/total)` format.
- **Options**:
  1. **Accept compact format** — Keep (done/total) as-is. Pros: Clean, works in narrow terminals. Cons: Doesn't match DESIGN.md.
  2. **Add ASCII progress bar** — e.g., `[===---] 60%`. Pros: Visual, matches design. Cons: Longer lines, terminal width concerns.
- **Recommendation**: Option 1 — compact format is clearer for CLI output
- **Rationale**: User-facing output format decision. Both options are valid.

### Story Summary/Context Type Breakdown — ESCALATE
- **Source**: VALIDATE-REPORT Finding #8
- **Severity**: Useful
- **Story**: SH-15
- **Description**: IDEA.md lists type breakdown in summary/context as a requirement. PLAN.md scope boundaries explicitly defer it.
- **Options**:
  1. **Accept as deferred scope** — Track as future enhancement. Pros: Clean scope boundary. Cons: Feature gap vs IDEA.md.
  2. **Implement type breakdown** — Add type distribution to summary/context. Pros: Fulfills requirement. Cons: Scope expansion.
- **Recommendation**: Option 1 — explicitly deferred during planning, should be its own story
- **Rationale**: Scope decision already made during planning. Should not be retro-fitted without deliberate planning.

## Resolved Items (No Action Needed)

These validate findings were resolved during the validate step (29 integration tests written):
- Validate #1: Integration tests for `story type list/add/remove` CLI commands
- Validate #2: Integration tests for `--type` flag on new/set/list
- Validate #3: Integration tests for `story epic` subcommands
- Validate #4: Integration tests for MCP tools with `story_type` parameter
- Validate #5: Integration test for progress rollup in human output
- Validate #6: E2E lifecycle test for full epic workflow

## Accepted Items (Correct Behavior)

- Validate #7: `story_type` field omitted (not null) in JSON for untyped stories — correct per `skip_serializing_if` pattern, consistent with other optional fields, verified by test
