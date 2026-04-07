# Implementation Plan: Replace MCP Server with CLI Documentation

## Summary

Remove the MCP server entirely. Replace it with:
1. A robust `story help --compact` / `story help --all` system for LLM-consumable CLI docs
2. A `story session-start` CLI command that outputs session context JSON (CLI reference + project state)
3. A rewritten session-start hook that calls `story session-start` (no more python3 dependency)
4. Enhanced `.storyhook/CLAUDE.md` and scaffold templates (MCP-free, CLI-first)

## Task Breakdown

### Wave 1 (no dependencies — parallel)

- [ ] Task 1.1: Strip MCP from Rust codebase
  - Acceptance:
    - `src/mcp.rs` and `src/mcp_install.rs` are deleted
    - `lib.rs` no longer declares `pub mod mcp` or `pub mod mcp_install`
    - `main.rs`: `--mcp` flag handling (lines 23-33) is removed entirely
    - `main.rs`: interactive McpConfig handler block (lines 48-71) is removed
    - `cli.rs`: `Invocation::McpConfig` variant is removed; `parse_mcp_config()` function is removed; `"mcp-config"` dispatch arm is removed; HELP_TEXT no longer contains "mcp-config" lines
    - `app.rs`: `McpConfig` match arm (lines 1424-1471) is removed; `resolve_binary_path()` function is removed (only caller was McpConfig handler)
    - `help_topics.rs`: `mcp-config` topic is removed; `json-format` topic no longer mentions MCP or JSON-RPC; the `json_format_topic_exists_and_covers_key_concepts` test no longer asserts on "JSON-RPC"
    - `tests/mcp_server.rs` is deleted
    - `tests/mcp_config.rs` is deleted
    - `tests/story_types.rs`: MCP test functions (lines ~749-834) and `mcp_request` helper are removed; all other tests remain
    - `cargo build` succeeds with no errors
    - `cargo test` passes (all non-MCP tests)
  - Files: `src/mcp.rs` (delete), `src/mcp_install.rs` (delete), `src/lib.rs`, `src/main.rs`, `src/cli.rs`, `src/app.rs`, `src/help_topics.rs`, `tests/mcp_server.rs` (delete), `tests/mcp_config.rs` (delete), `tests/story_types.rs`

- [ ] Task 1.2: Remove MCP from documentation and plugin files
  - Acceptance:
    - `README.md`: MCP Server section (lines ~287-325) is removed; bullet point "MCP server for native AI tool integration" is removed; no remaining references to MCP, `--mcp`, or `mcp-config`
    - `install.sh`: MCP registration section (lines ~72-127) is removed; post-install hint line mentioning `story mcp-config` is removed; install script still works for binary install + git hooks
    - `plugin/claude-code/references/cli-reference.md`: `story mcp-config` section (lines ~463-477) is removed
    - No file in the repository (outside `.forge/`) contains the string "mcp-config" (case-insensitive)
  - Files: `README.md`, `install.sh`, `plugin/claude-code/references/cli-reference.md`

### Wave 2 (depends on Wave 1 — code must compile)

- [ ] Task 2.1: Add `--compact` and `--all` flags to help system
  - Acceptance:
    - `story help --compact` outputs a curated LLM-optimized CLI reference:
      - Contains key command names: `init`, `new`, `list`, `next`, `show`, `move`, `comment`, `assign`, `prioritize`, `label`, `block`, `unblock`, `relate`, `search`, `summary`, `context`, `decompose`, `graph`, `handoff`, `set`, `help`
      - Output is between 40 and 100 lines (concise enough for context injection)
      - Output is under 3000 characters
      - Includes brief "when to use" guidance for core commands
      - Includes the `--json` global flag
      - Does NOT include lengthy examples or full option documentation
    - `story help --all` outputs all help topics as a single document:
      - Contains content from every topic in `help_topics.rs`
      - Is at least 3x longer than `--compact` output
      - Topics are separated by clear headers
    - `story help --compact --json` outputs JSON envelope with `"message"` field containing the compact reference
    - `story help --all --json` outputs JSON envelope with `"message"` field containing all topics
    - `story help <topic>` continues to work for all existing topics (backward compatible)
    - `story help` with no args continues to list available topics
    - New tests verify compact output contains key commands, respects size limits, and --all includes all topics
  - Files: `src/cli.rs` (modify `parse_help`, add flags to `Invocation::HelpTopic` or add new variants), `src/app.rs` (add handlers), `tests/cli_help.rs` (add tests)

### Wave 3 (depends on Wave 2 — parallel tasks)

- [ ] Task 3.1: Add `story session-start` CLI command
  - Acceptance:
    - `story session-start` (when `.storyhook/` exists and plugin is enabled):
      - Outputs valid JSON with a `"systemMessage"` string field
      - The systemMessage contains the compact CLI reference (same content as `story help --compact`)
      - The systemMessage contains project state: open story count, ready story count
      - The systemMessage contains next story info (ID, title, priority) when a ready story exists
      - Total systemMessage is under 4000 characters
    - `story session-start` (when `.storyhook/` does NOT exist):
      - Outputs `{}` (empty JSON object)
    - `story session-start` (when `.storyhook/plugin-config.toml` has `enabled = false`):
      - Outputs `{}` (empty JSON object)
    - `story session-start` (when `.storyhook/` exists but no stories):
      - Outputs valid JSON with systemMessage containing CLI reference and "0 open stories"
    - Command handles special characters in story titles (quotes, backslashes, unicode) without breaking JSON
    - Command completes in under 2 seconds
    - New integration tests verify all above scenarios
  - Files: `src/cli.rs` (add `Invocation::SessionStart`, parser), `src/app.rs` (add handler), `src/help_topics.rs` (add `session-start` topic), `tests/session_start.rs` (new)

- [ ] Task 3.2: Update scaffold templates and CLAUDE.md
  - Acceptance:
    - `generate_storyhook_claude_md()` in `app.rs`:
      - No longer contains "MCP Server" section
      - References `story help <command>` for detailed usage
      - References `story help --compact` for quick reference
      - Is more comprehensive: includes relationship types, decompose workflow, graph usage
    - `generate_agents_md()` in `app.rs`:
      - No MCP references
      - References `story help --compact` for full reference
    - `generate_claude_md()` in `app.rs`:
      - No MCP references
    - `generate_cursor_rules()` in `app.rs`:
      - No MCP references (removes "or MCP server" and "MCP Server" section)
      - References `story help <command>` for more info
    - `story scaffold agents-md` output does not contain "MCP" or "mcp"
    - `story scaffold cursor-rules` output does not contain "MCP" or "mcp"
    - `story scaffold claude-md` output does not contain "MCP" or "mcp"
    - `story init` creates `.storyhook/CLAUDE.md` without MCP references
    - Existing scaffold tests pass; new assertions check for absence of MCP
  - Files: `src/app.rs` (scaffold template functions), `tests/scaffold.rs` (update assertions), `tests/init_command.rs` (verify CLAUDE.md content)

### Wave 4 (depends on Task 3.1)

- [ ] Task 4.1: Rewrite session-start hook
  - Acceptance:
    - `plugin/claude-code/hooks/session-start.sh` is rewritten to:
      - Call `story session-start` (single CLI invocation, no python3)
      - Output the JSON result directly
      - Fall back to `{}` if `story` binary is not found or command fails
    - Hook is under 20 lines (simple wrapper)
    - No `python3` dependency
    - Hook completes within the 5-second timeout configured in `hooks.json`
    - `plugin/claude-code/references/cli-reference.md` documents `story session-start`
    - `plugin/claude-code/references/workflow-patterns.md` has no MCP references
  - Files: `plugin/claude-code/hooks/session-start.sh`, `plugin/claude-code/references/cli-reference.md`, `plugin/claude-code/references/workflow-patterns.md`

## Test Strategy

### New Tests
| File | Tests | What They Verify |
|------|-------|-----------------|
| `tests/cli_help.rs` (extend) | 6+ | `--compact` output contains key commands, respects size limits; `--all` contains all topics, is 3x+ longer than compact; JSON output works; backward compat preserved |
| `tests/session_start.rs` (new) | 8+ | Valid JSON output; systemMessage content; empty project handling; disabled plugin handling; no-project handling; special character/unicode safety; performance (under 2s) |
| `tests/scaffold.rs` (extend) | 3+ | All three scaffold variants are MCP-free |
| `tests/init_command.rs` (extend) | 1+ | Generated CLAUDE.md is MCP-free |

### Deleted Tests
| File | Tests | Reason |
|------|-------|--------|
| `tests/mcp_server.rs` (delete) | ~8 | MCP server removed |
| `tests/mcp_config.rs` (delete) | ~7 | mcp-config command removed |
| `tests/story_types.rs` (modify) | 3-4 | MCP-specific type tests removed; ~30 other tests preserved |

### Existing Tests
All non-MCP tests must continue to pass. Hard MCP removal does not affect any existing test because no test (outside the deleted files) exercises the `--mcp` flag.

## Resumption Points

- **After Wave 1**: MCP is gone, code compiles, all non-MCP tests pass. Safe to pause.
- **After Wave 2**: Help system enhanced. `story help --compact` and `--all` work. Safe to pause.
- **After Wave 3**: Session-start command works, templates are clean. Safe to pause.
- **After Wave 4**: Hook rewritten, full integration working. Done.

## Risk Register

| Risk | Impact | Likelihood | Mitigation |
|------|--------|-----------|------------|
| Stale MCP registrations in `~/.claude.json` | None | N/A | Solo user — will clean up manually. Hard removal, no deprecation stub. |
| Cursor/Codex/Antigravity users lose MCP integration | Medium | Possible | These tools can still use the CLI directly. Scaffold templates (`story scaffold cursor-rules`, `story scaffold agents-md`) provide integration guidance. MCP was a convenience, not the only path. |
| `--compact` help is wrong size (too long wastes context, too short is useless) | Low | Possible | Size constraints are in acceptance criteria (40-100 lines, <3000 chars). Can be tuned after initial implementation. |
| Plugin distribution doesn't work for `cargo install` (pre-existing issue) | Low | Known | Out of scope — this is a pre-existing limitation of `story plugin install claude-code`. The `.storyhook/CLAUDE.md` file (auto-created by `story init`) provides baseline LLM guidance regardless of plugin status. |
| `python3` removal from hook breaks fallback behavior | None | N/A | Eliminated by design — `story session-start` is a native Rust command, no external dependencies. |

## Skeptic Findings Addressed

1. **Stale MCP registrations** → Hard removal; solo user will clean up manually
2. **Python3 dependency in hook** → Eliminated via `story session-start` CLI command
3. **Hook JSON parsing bugs** → Eliminated (no more shell-based JSON parsing)
4. **Non-Claude-Code tools** → Scaffold templates provide CLI-first integration for all tools
5. **Compact help size** → Acceptance criteria define bounds (40-100 lines, <3000 chars)
6. **Plugin distribution** → Pre-existing issue, out of scope; `.storyhook/CLAUDE.md` is the baseline
7. **Breaking change versioning** → This is v0.x software; minor version bump appropriate
