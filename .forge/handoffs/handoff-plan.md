# Handoff: Plan → Decompose

## Step Completed
plan

## Artifacts Produced
- `.forge/PLAN.md` — Full implementation plan with 8 tasks across 4 waves, 35 requirements, 50-test strategy, 10-item risk register

## Key Decisions
- **8 tasks, 4 waves** — T1.1+T1.3 parallel → T1.2+T2.2 parallel → T2.1 → T3.1+T3.2+T3.3 parallel → T4.1
- **T3.3 added (export/import)** — Devil's advocate identified that ProjectExport must include types.toml and ImportStory needs story_type field. Without this, export/import silently loses type config.
- **remove_type checks ALL stories** — Open + archived, matching remove_state precedent. Prevents doctor integrity issues after type removal.
- **Progress rollup computed unconditionally** in build_story_views — no new flag, cost negligible vs existing disk I/O.
- **50 integration tests** — No mocks, real binary against real filesystem. tests/story_types.rs following assert_cmd + tempdir pattern.
- **T2.1 is the largest task** — Optional split point into T2.1a (type handlers) and T2.1b (epic handlers) if generator session is insufficient.
- **Plan approved by user** — No adjustments requested.

## Context for Next Step

### Task Summary
| Task | Files | Scope | Dependencies |
|------|-------|-------|-------------|
| T1.1 | domain.rs | StoryTypeSet event, TypeDef, snapshot, fold, ImportStory, ProgressRollup | none |
| T1.2 | storage.rs | types.toml lifecycle (load/save/add/remove/ensure/default) | T1.1 |
| T1.3 | cli.rs | TypeAction, EpicAction, Invocation variants, --type flag, parsers | none |
| T2.1 | app.rs | Type/Epic handlers, validation, --type filter, epic create two-event | T1.1, T1.2, T1.3 |
| T2.2 | output.rs | StoryView.progress, type + progress rendering | T1.1 |
| T3.1 | domain.rs, app.rs | has_children, compute_progress, build_story_views, Next skip, doctor | T2.1, T2.2 |
| T3.2 | mcp.rs | story_type param on create/update/list MCP tools | T1.3 |
| T3.3 | storage.rs, app.rs, mcp.rs | Export/import types.toml, ImportStory type handling, bulk_create | T1.1, T1.2 |
| T4.1 | all | cargo build + test + clippy verification | T3.1, T3.2, T3.3 |

### Decomposition Guidance
- Each task maps to 1 storyhook story
- Wave boundaries define story dependencies (blocked-by relationships)
- T2.1 may need to be split into 2 stories if too large for one generator session
- Acceptance criteria are machine-evaluable — suitable for evaluator agent

### Patterns to Follow (for story descriptions)
- StoryPrioritySet → StoryTypeSet (domain.rs:184-187)
- states.toml loading → types.toml loading (storage.rs:277-344)
- StateAdd/StateRemove → TypeAdd/TypeRemove (app.rs:72-88)
- story phase → story type/story epic (cli.rs dispatch)

## Pipeline State
- Fix cycle: 0 / 3
- Yolo mode: false
- Team roster: core only (no conditional agents)
- ESCALATE stories pending: 0
- Plan approved: yes, no adjustments

## Open Questions for Decompose
1. Should T2.1 be split into 2 stories (type handlers vs epic handlers)?
2. Should test writing be a separate story per wave or integrated into each task's story?
