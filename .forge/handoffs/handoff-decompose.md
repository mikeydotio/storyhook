# Handoff: Decompose → Execute

## Step Completed
decompose

## Artifacts Produced
- `.forge/plan-mapping.json` — 6 task stories (SH-32 through SH-37) across 4 waves, parent SH-31

## Key Decisions
- **Flat decomposition** — Created parent story SH-31 manually, then used `storyhook_decompose_spec` for wave structure. Avoids unnecessary two-level hierarchy.
- **Stale artifacts cleaned** — Removed old `state.json` from previous pipeline (Story Types & Epics fix cycle 3). Old `plan-mapping.json` overwritten with new mapping.
- **No DESIGN.md for new pipeline** — The `.forge/DESIGN.md` is from the old Story Types & Epics pipeline. Implementation context embedded directly in `plan-mapping.json` design_section fields from the detailed PLAN.md.
- **DAG validated** — No cycles. Critical path: SH-33 → SH-34 → SH-35 → SH-37 (4 stories).

## Story-to-Task Mapping

| Story | Task | Wave | Priority | Title |
|-------|------|------|----------|-------|
| SH-32 | T1.1 | 1 | high | Strip MCP from Rust codebase |
| SH-33 | T1.2 | 1 | high | Remove MCP from docs/plugin files |
| SH-34 | T2.1 | 2 | high | Add --compact and --all help flags |
| SH-35 | T3.1 | 3 | high | Add session-start CLI command |
| SH-36 | T3.2 | 3 | medium | Update scaffold templates (MCP-free) |
| SH-37 | T4.1 | 4 | medium | Rewrite session-start hook |

## Wave Dependencies
- Wave 1 (SH-32, SH-33): No dependencies, parallel
- Wave 2 (SH-34): Blocked by SH-32, SH-33
- Wave 3 (SH-35, SH-36): Blocked by SH-34, parallel
- Wave 4 (SH-37): Blocked by SH-35, SH-36

## Context for Next Step
- 6 stories, all in `todo` state, ready for execution
- Wave 1 has 2 ready stories (SH-32, SH-33) — can start immediately
- This is fix_cycle 0 (fresh pipeline, not a fix loop)
- All test infrastructure from previous pipeline is in place (states.toml has in-progress, verifying, blocked)
- 3 untracked test files exist from previous plan work: tests/help_new_flags.rs, tests/mcp_removal.rs, tests/session_start_hook.rs

## Pipeline State
- Fix cycle: 0
- Yolo mode: false
- ESCALATE stories pending: 0

## Open Questions
None — stories decomposed, DAG valid, ready for execution.
