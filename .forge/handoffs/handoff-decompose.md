# Handoff: Decompose → Execute (Fix Cycle 5)

## Summary
Decomposition complete. 5 ESCALATE stories already existed from triage — built plan-mapping.json linking them to PLAN.md tasks. All stories are in `todo` state, single wave, no dependencies.

## Key Decisions
- Stories pre-existed from ESCALATE review (SH-34, SH-35, SH-36, SH-38, SH-39)
- Parent story: SH-40
- No new stories created — mapping only
- DAG validated: 0 dependency edges, no cycles, all stories independent
- Design sections sourced from story comments (user decisions) rather than DESIGN.md (which covers types/epics feature, not fix cycle work)

## Context for Next Step (Execute)
- 5 stories, all in wave 1 (parallel, no dependencies between them)
- All touch different files — no merge conflict risk
- All are text/config changes — no new logic paths
- Test strategy: `cargo build` + `cargo test` + grep checks per acceptance criteria
- SH-36 is the most complex (2 files: Cargo.toml sync + new post-bump hook script)

### Story → File Mapping
| Story | Files |
|-------|-------|
| SH-34 | src/cli.rs |
| SH-35 | src/storage.rs |
| SH-36 | Cargo.toml, .semver/hooks/post-bump/sync-cargo-toml.sh |
| SH-38 | CHANGELOG.md |
| SH-39 | src/plugin.rs |

### Critical Implementation Notes
- SH-35: Remove the --tree line, do NOT replace with --blocked-by (wrong semantics)
- SH-36: Use post-bump hook, NOT config.yaml tracked_files (doesn't exist)
- SH-36: Hook must strip `v` prefix from $NEW_VERSION
- SH-38: ### Removed goes after ### Changed, before _[manual]_ marker

## Open Questions
None — all decisions made during ESCALATE review and plan approval.
