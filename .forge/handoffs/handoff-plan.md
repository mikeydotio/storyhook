# Handoff: Plan → Decompose (Fix Cycle 5)

## Summary
Plan approved for fix cycle 5. 5 ESCALATE stories, single wave, all independent. User approved the SH-36 correction (post-bump hook instead of non-existent config key).

## Key Decisions
- All 5 tasks in Wave 1 (parallel, no dependencies)
- SH-36 uses post-bump hook approach, not config.yaml tracked_files (which doesn't exist)
- No new test files — these are text/config changes verified by cargo build + grep
- CHANGELOG `### Removed` section placed after `### Changed`, before `_[manual]_` marker

## Context for Next Step (Decompose)
- Stories already exist in storyhook: SH-34, SH-35, SH-36, SH-38, SH-39
- All have acceptance criteria in comments from triage
- SH-36 acceptance criteria need updating to reflect hook approach (not config key)
- Each story maps 1:1 to a plan task — decompose should be straightforward
- All stories are children of SH-40

### Task → Story Mapping
| Task | Story | Files |
|------|-------|-------|
| T1.1 | SH-34 | src/cli.rs |
| T1.2 | SH-35 | src/storage.rs |
| T1.3 | SH-36 | Cargo.toml, .semver/hooks/post-bump/sync-cargo-toml.sh |
| T1.4 | SH-38 | CHANGELOG.md |
| T1.5 | SH-39 | src/plugin.rs |

### Critical Implementation Notes
- SH-36: Semver plugin has no tracked_files config key; use .semver/hooks/post-bump/ hook scripts
- SH-36: Hook must strip `v` prefix from $NEW_VERSION before updating Cargo.toml
- SH-38: `### Removed` goes after `### Changed` (line 24) and before `_[manual]_` (line 26)
- All tasks touch different files — no merge conflict risk

## Pipeline State
- Fix cycle: 5 (exceeds default max of 3 — inherited from cycle 4 ESCALATE review)
- Yolo: false
- Stories SH-31, SH-32, SH-33 completed in earlier fix cycles (FIX items)
- SH-37 closed as no-action in cycle 4

## Open Questions
None — all decisions made during ESCALATE review and plan approval.
