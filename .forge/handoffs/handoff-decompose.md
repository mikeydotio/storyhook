# Handoff: Decompose → Execute

## Step Completed
decompose

## Artifacts Produced
- `.forge/plan-mapping.json` — 9 task stories + 1 parent, mapped to DESIGN.md sections
- `.storyhook/states.toml` — Added `in-progress`, `verifying`, `blocked` states for execution loop

## Key Decisions
- **9 task stories across 5 waves** — T1.1+T1.3 (wave 1) → T1.2+T2.2 (wave 2) → T2.1 (wave 3) → T3.1+T3.2+T3.3 (wave 4) → T4.1 (wave 5)
- **T2.1 kept as single story** — not split into type/epic handlers. Coherent as-is per plan guidance.
- **Wave-based dependencies are conservative** — decompose_spec creates "all wave N blocked-by all wave N-1". T3.2 (MCP, SH-9) and T3.3 (export/import, SH-10) have stricter blockers than strictly needed (blocked-by SH-7/T2.1, but T3.2 only needs T1.3 and T3.3 only needs T1.1+T1.2). This is safe but prevents some parallelism.
- **SH-1 is umbrella parent**, SH-2 is decomposed parent (child-of SH-1), SH-3–SH-11 are task stories (children of SH-2)
- **DAG validated** — no cycles. Critical path: SH-4 → SH-5 → SH-7 → SH-10 → SH-11 (5 stories deep)
- **Descriptions stored as storyhook comments** — acceptance criteria embedded in story comments, also in plan-mapping.json

## Context for Next Step

### Story-to-Task Mapping
| Story | Task | Wave | Files | Scope |
|-------|------|------|-------|-------|
| SH-3 | T1.1 | 1 | domain.rs | medium |
| SH-4 | T1.3 | 1 | cli.rs | medium |
| SH-5 | T1.2 | 2 | storage.rs | medium |
| SH-6 | T2.2 | 2 | output.rs | medium |
| SH-7 | T2.1 | 3 | app.rs | large |
| SH-8 | T3.1 | 4 | domain.rs, app.rs | medium |
| SH-9 | T3.2 | 4 | mcp.rs | small |
| SH-10 | T3.3 | 4 | storage.rs, app.rs, mcp.rs | small |
| SH-11 | T4.1 | 5 | all | small |

### Execution Ordering
Wave 1 stories (SH-3, SH-4) are ready immediately — no blockers. Execute one at a time per forge rules. SH-3 first (T1.1 domain.rs) is recommended since it unblocks the most downstream stories.

### Patterns for Generator
- StoryPrioritySet → StoryTypeSet (domain.rs:184-187)
- states.toml loading → types.toml loading (storage.rs:277-344)
- StateAdd/StateRemove → TypeAdd/TypeRemove (app.rs:72-88)
- story phase → story type/story epic (cli.rs dispatch)

## Pipeline State
- Fix cycle: 0 / 3
- Yolo mode: false
- Stories: 9 task stories (all todo), 2 parent stories
- Ready stories: SH-3 (T1.1), SH-4 (T1.3)

## Open Questions
- None — decomposition is straightforward. All tasks map 1:1 to stories.
