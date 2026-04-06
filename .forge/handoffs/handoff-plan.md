# Handoff: Plan → Decompose

## Step Completed
plan

## Artifacts Produced
- `.forge/PLAN.md` — 7 tasks across 4 waves, approved by user

## Key Decisions
- **Hard MCP removal** — No deprecation stub for `--mcp` flag. Solo user, no backward compat needed. Flag and command removed entirely.
- **`story session-start` command** — New CLI command replaces python3-heavy shell script. Outputs `{"systemMessage": "..."}` with compact CLI reference + project state. Eliminates external dependencies.
- **`story help --compact`** — LLM-optimized reference, 40-100 lines, <3000 chars. Injected into sessions via `story session-start`.
- **`story help --all`** — Full topic dump as single document. For comprehensive context when needed.
- **Wave structure**: Strip MCP → Add help flags → Add session-start + clean templates → Rewrite hook
- **Test strategy**: Delete 2 test files (mcp_server.rs, mcp_config.rs), remove 3-4 MCP tests from story_types.rs, add ~15+ new tests across cli_help.rs, session_start.rs, scaffold.rs, init_command.rs

## Context for Next Step

### Task Count and Dependencies
- 7 tasks total across 4 waves
- Wave 1: T1.1 (Rust MCP strip) + T1.2 (doc MCP strip) — parallel
- Wave 2: T2.1 (help --compact/--all) — depends on Wave 1
- Wave 3: T3.1 (session-start cmd) + T3.2 (template cleanup) — parallel, depends on Wave 2
- Wave 4: T4.1 (hook rewrite) — depends on T3.1

### Critical Implementation Notes
- `resolve_binary_path()` in app.rs is only used by McpConfig handler — delete it
- `json_format_topic_exists_and_covers_key_concepts` test asserts on "JSON-RPC" — remove that assertion
- `tests/story_types.rs` has MCP tests at lines ~749-834 with `mcp_request` helper — remove
- `dialoguer` dependency stays (used by github sync, not just MCP)
- Current session-start hook has bugs: misparses `story summary --json` output format — `story session-start` fixes this
- `.storyhook/CLAUDE.md` template is in `generate_storyhook_claude_md()` in app.rs

### Files Changed Summary
- Delete: src/mcp.rs, src/mcp_install.rs, tests/mcp_server.rs, tests/mcp_config.rs
- Modify (MCP strip): lib.rs, main.rs, cli.rs, app.rs, help_topics.rs, story_types.rs
- Modify (docs): README.md, install.sh, cli-reference.md
- Modify (new features): cli.rs, app.rs, help_topics.rs, cli_help.rs
- Create: tests/session_start.rs
- Modify (templates): app.rs scaffold functions, scaffold.rs, init_command.rs
- Modify (hook): session-start.sh, cli-reference.md, workflow-patterns.md

## Pipeline State
- Fix cycle: 0
- Yolo mode: false
- ESCALATE stories pending: 0

## Open Questions
None — plan approved with hard MCP removal, ready for decomposition.
