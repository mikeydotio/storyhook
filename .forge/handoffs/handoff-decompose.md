# Handoff: Decompose → Execute (FIX Cycle 4)

## Step Completed
decompose (FIX cycle 4 — plan → decompose)

## Artifacts Produced
- `.forge/plan-mapping.json` — 8 stories mapped to plan tasks, parent SH-40

## Key Decisions
- Created parent story SH-40 "Fix Cycle 4" with parent-of relationships to all 8 fix stories
- No new stories created — mapped existing ESCALATE stories (SH-31–SH-39, excluding done SH-37) to plan tasks
- DAG validated: no cycles, all 8 stories are independent roots (Wave 1)
- Design sections embedded from PLAN.md fix descriptions (not DESIGN.md — this is a fix cycle, not feature work)

## Story-to-Task Mapping

| Story | Task | Wave | Priority | Title |
|-------|------|------|----------|-------|
| SH-31 | T1.1 | 1 | critical | UTF-8 safe truncation |
| SH-32 | T1.2 | 1 | high | Proper TOML parsing for plugin config |
| SH-33 | T1.3 | 1 | high | RawJson bypasses --quiet |
| SH-34 | T1.4 | 1 | medium | Fix HELP_TEXT |
| SH-35 | T1.5 | 1 | medium | Remove ghost --tree reference |
| SH-36 | T1.6 | 1 | medium | Sync VERSION and Cargo.toml versions |
| SH-38 | T1.7 | 1 | medium | Add CHANGELOG entry for MCP removal |
| SH-39 | T1.8 | 1 | low | Add CLI alternative to plugin install message |

## Wave Dependencies
- Wave 1 (all 8 stories): No cross-dependencies, all independent

## Context for Next Step (Execute)

### Critical Implementation Notes
- **SH-31 + SH-32 both modify src/app.rs** — assign to same generator to avoid merge conflicts
- **SH-32**: Must handle both bare-key (`enabled = "false"`) AND [plugin] table formats; preserve fail-open behavior
- **SH-32**: Existing bug-documenting test needs updating to assert fixed behavior
- **SH-36**: Use .semver/hooks/post-bump/ hook scripts (semver plugin has no tracked_files config key)

### Test Expectations
- 6 new tests, 1 updated test
- SH-31: 1 new (multi-byte UTF-8 truncation)
- SH-32: 3 new (no-space, comments, nested table) + 1 updated (whitespace bug → fixed)
- SH-33: 1 new (--quiet with session-start)
- SH-36: 1 new (version sync)

## Pipeline State
- Fix cycle: 4 (3 prior cycles archived in fix-cycles/)
- Parent story: SH-40
- Yolo mode: false

## Open Questions
None — stories decomposed, DAG valid, ready for execution.
