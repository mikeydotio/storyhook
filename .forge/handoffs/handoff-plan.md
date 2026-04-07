# Handoff: Plan → Decompose (FIX Cycle 4)

## Step Completed
plan (FIX cycle 4 — ESCALATE review → plan)

## Artifacts Produced
- `.forge/PLAN.md` — 8 tasks in 1 wave, approved by user

## Key Decisions
- All 9 ESCALATE stories reviewed; 8 approved for fix, 1 (SH-37) accepted as no-action and closed
- Single wave — all 8 fixes are independent
- T1.1 + T1.2 must be assigned to same generator (both modify src/app.rs)
- SH-32 TOML fix must handle both bare-key and [plugin] section formats (skeptic identified test-breaking risk)
- SH-36 uses post-bump hook (semver plugin has no config key for tracked files)
- 6 new tests + 1 updated test (whitespace bug test flipped to assert fixed behavior)

## Context for Next Step (Decompose)

### Task → Story Mapping
Stories already exist in storyhook — decompose should map plan tasks to existing stories, no new story creation needed.

| Task | Story | Fix |
|------|-------|-----|
| T1.1 | SH-31 | floor_char_boundary(3900) one-liner |
| T1.2 | SH-32 | toml crate parsing, both config formats, fail-open preserved |
| T1.3 | SH-33 | reorder RawJson before quiet check |
| T1.4 | SH-34 | update HELP_TEXT string |
| T1.5 | SH-35 | remove ghost --tree line entirely |
| T1.6 | SH-36 | sync Cargo.toml to 0.12.0 + create post-bump hook |
| T1.7 | SH-38 | add ### Removed section to CHANGELOG under v0.12.0 |
| T1.8 | SH-39 | add CLI alternative alongside skill reference |

### Critical Implementation Notes
- SH-32: Must handle bare-key (`enabled = "false"`) AND [plugin] table (`[plugin]\nenabled = false`) formats
- SH-32: Malformed/missing config → fail-open (treat as enabled) — existing test guards this
- SH-32: Existing bug-documenting test `session_start_plugin_config_extra_whitespace_bug_documented` needs updating
- SH-36: Semver plugin has no tracked_files config key; use .semver/hooks/post-bump/ hook scripts
- T1.1 + T1.2 touch src/app.rs — group to same generator

### Files Changed Summary
- src/app.rs (T1.1, T1.2)
- src/output.rs (T1.3)
- src/cli.rs (T1.4)
- src/storage.rs (T1.5)
- Cargo.toml, .semver/hooks/post-bump/sync-cargo-toml.sh (T1.6)
- CHANGELOG.md (T1.7)
- src/plugin.rs (T1.8)
- tests/session_start.rs (T1.1, T1.2, T1.3 — new/updated tests)
- tests/version_sync.rs (T1.6 — new test file)

## Pipeline State
- Fix cycle: 4 (3 prior cycles archived in fix-cycles/)
- Yolo mode: false
- Stories: 8 open ESCALATE stories ready for decompose (SH-37 closed)

## Open Questions
None — plan approved, ready for decomposition.
