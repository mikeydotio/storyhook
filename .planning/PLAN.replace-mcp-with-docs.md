# Implementation Plan: Replace MCP Server with Documentation-Based CLI Integration

**Project:** storyhook v0.12.0
**Date:** 2026-04-06
**Scope:** Remove MCP server, add help --compact/--all, rewrite session-start hook

---

## Requirements

| ID | Requirement | Type | Priority |
|----|-------------|------|----------|
| R1 | Delete MCP server source (`src/mcp.rs`) | functional | high |
| R2 | Delete MCP installer source (`src/mcp_install.rs`) | functional | high |
| R3 | Delete MCP test files (`tests/mcp_server.rs`, `tests/mcp_config.rs`) | functional | high |
| R4 | Remove `pub mod mcp; pub mod mcp_install;` from `src/lib.rs` | functional | high |
| R5 | Remove `--mcp` flag handling from `src/main.rs` | functional | high |
| R6 | Remove `McpConfig` interactive handler from `src/main.rs` | functional | high |
| R7 | Remove `Invocation::McpConfig` variant and `parse_mcp_config()` from `src/cli.rs` | functional | high |
| R8 | Remove `mcp-config` from HELP_TEXT in `src/cli.rs` | functional | high |
| R9 | Remove `mcp-config` dispatch entry from `src/cli.rs` | functional | high |
| R10 | Remove `McpConfig` handler from `src/app.rs` (lines 1424-1471) | functional | high |
| R11 | Remove `resolve_binary_path()` from `src/app.rs` (only caller is McpConfig handler) | functional | high |
| R12 | Remove MCP references from `generate_storyhook_claude_md()` scaffold template in `src/app.rs` | functional | high |
| R13 | Remove MCP references from `generate_cursor_rules()` scaffold template in `src/app.rs` | functional | high |
| R14 | Remove `mcp-config` topic from `src/help_topics.rs` | functional | high |
| R15 | Remove MCP section from `json-format` topic in `src/help_topics.rs` | functional | high |
| R16 | Update `json_format_topic_exists_and_covers_key_concepts` test to remove MCP assertion | functional | high |
| R17 | Remove MCP section from `README.md` (lines 287-325) | functional | high |
| R18 | Remove MCP registration section from `install.sh` (lines 72-101) and MCP fallback message (line 127) | functional | high |
| R19 | Remove `mcp-config` section from `plugin/claude-code/references/cli-reference.md` | functional | high |
| R20 | Remove MCP test functions from `tests/story_types.rs` (lines 749-834) | functional | high |
| R21 | Add `--compact` flag to `story help` that outputs LLM-friendly single-page CLI reference | functional | high |
| R22 | Add `--all` flag to `story help` that dumps all help topics | functional | high |
| R23 | Rewrite `plugin/claude-code/hooks/session-start.sh` to inject CLI reference + project state | functional | high |
| R24 | Update `generate_storyhook_claude_md()` to be more comprehensive (remove MCP, add CLI-first guidance) | functional | medium |
| R25 | Project compiles successfully after each wave | non-functional | high |
| R26 | All existing tests pass (except deleted MCP tests) | non-functional | high |
| R27 | `dialoguer` dependency remains (used by GitHub sync) | non-functional | high |
| R28 | Session-start hook outputs valid `{"systemMessage": "..."}` JSON | non-functional | high |

---

## Task Waves

### Wave 1 (parallel -- no dependencies)

These tasks are independent. Each removes or modifies a distinct set of files with no cross-task data flow. The constraint is that all Wave 1 tasks must be complete before the project will compile (the MCP modules must be deleted at the same time as their references are removed).

**IMPORTANT:** Because Rust compilation is all-or-nothing, Wave 1 tasks must all land in a single commit or be applied atomically. An executing agent should treat Wave 1 as a single atomic unit. The sub-tasks below are listed for traceability and to define the scope of changes, but they are executed together.

#### T1.1: Remove MCP modules and all Rust references
- **Requirement(s)**: R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12, R13, R14, R15, R16, R20, R25, R26, R27
- **Acceptance criteria**:
  - [ ] `src/mcp.rs` does not exist
  - [ ] `src/mcp_install.rs` does not exist
  - [ ] `tests/mcp_server.rs` does not exist
  - [ ] `tests/mcp_config.rs` does not exist
  - [ ] `src/lib.rs` does not contain the strings `pub mod mcp` or `pub mod mcp_install`
  - [ ] `src/main.rs` does not contain the string `--mcp`
  - [ ] `src/main.rs` does not contain the string `McpConfig`
  - [ ] `src/main.rs` does not contain the string `mcp_install`
  - [ ] `src/cli.rs` does not contain the string `McpConfig`
  - [ ] `src/cli.rs` does not contain the string `mcp-config` (in HELP_TEXT, dispatch, or parser)
  - [ ] `src/cli.rs` does not contain the function `parse_mcp_config`
  - [ ] `src/app.rs` does not contain the string `McpConfig`
  - [ ] `src/app.rs` does not contain the function `resolve_binary_path`
  - [ ] `src/app.rs` `generate_storyhook_claude_md()` does not contain the string `MCP` or `mcp`
  - [ ] `src/app.rs` `generate_cursor_rules()` does not contain the string `MCP` or `mcp`
  - [ ] `src/help_topics.rs` does not contain the string `mcp-config` as a topic key
  - [ ] `src/help_topics.rs` `json-format` topic does not contain the string `MCP` or `JSON-RPC`
  - [ ] The test `json_format_topic_exists_and_covers_key_concepts` does not assert on `JSON-RPC`
  - [ ] `tests/story_types.rs` does not contain functions `mcp_request`, `mcp_create_story_with_type`, `mcp_update_story_type`, or `mcp_list_stories_with_type_filter`
  - [ ] `cargo build` succeeds with zero errors
  - [ ] `cargo test` passes with zero failures
  - [ ] `dialoguer` is still listed in `Cargo.toml` dependencies
- **Files deleted**: `src/mcp.rs`, `src/mcp_install.rs`, `tests/mcp_server.rs`, `tests/mcp_config.rs`
- **Files modified**: `src/lib.rs`, `src/main.rs`, `src/cli.rs`, `src/app.rs`, `src/help_topics.rs`, `tests/story_types.rs`
- **Estimated scope**: large (many files, but all mechanical deletions/removals)

#### T1.2: Remove MCP from documentation and install script
- **Requirement(s)**: R17, R18, R19
- **Acceptance criteria**:
  - [ ] `README.md` does not contain the heading `## MCP Server`
  - [ ] `README.md` does not contain the string `story --mcp`
  - [ ] `README.md` does not contain the string `mcp-config`
  - [ ] `install.sh` does not contain the string `MCP` (case-sensitive)
  - [ ] `install.sh` does not contain the string `mcp-config`
  - [ ] `install.sh` still contains the git hooks section (lines 102-123 region, which should remain)
  - [ ] `install.sh` non-interactive fallback line referencing MCP is removed; the line `echo "To install git hooks:         story hooks install"` remains
  - [ ] `plugin/claude-code/references/cli-reference.md` does not contain the heading `### \`story mcp-config`
  - [ ] `plugin/claude-code/references/cli-reference.md` does not contain the string `mcp`
- **Files modified**: `README.md`, `install.sh`, `plugin/claude-code/references/cli-reference.md`
- **Estimated scope**: small

---

### Wave 2 (depends on Wave 1)

Wave 2 tasks add new features. They depend on Wave 1 because:
- T2.1 modifies the same `Invocation` enum and `parse_help` function that T1.1 cleaned up
- T2.2 modifies `src/app.rs` help handler that T1.1 cleaned up
- T2.3 depends on `story help --compact` (T2.1 + T2.2) existing

T2.1 and T2.2 are tightly coupled (parser + handler) and should be done together.

#### T2.1: Add `--compact` and `--all` flags to help system (parser + handler)
- **Depends on**: T1.1
- **Requirement(s)**: R21, R22, R25, R26
- **Acceptance criteria**:
  - [ ] `src/cli.rs` `Invocation::HelpTopic` variant includes a `mode` field (or the enum has new variants `HelpCompact` and `HelpAll`) to distinguish `--compact` and `--all` from normal topic help
  - [ ] `story help --compact` parses without error (verified by unit test or `cargo test`)
  - [ ] `story help --all` parses without error
  - [ ] `story help <topic>` still works unchanged
  - [ ] `story help` (no args) still shows the general HELP_TEXT
  - [ ] `src/app.rs` handles `story help --compact` and returns a single-page, LLM-optimized CLI reference containing: (a) all command signatures in a compact table/list, (b) common workflows as brief examples, (c) total output under ~150 lines
  - [ ] `src/app.rs` handles `story help --all` and returns concatenated output of all help topics
  - [ ] `story help --compact` output contains the string `story new` (verifies commands are listed)
  - [ ] `story help --compact` output contains the string `story list` (verifies commands are listed)
  - [ ] `story help --compact` output contains the string `story move` (verifies commands are listed)
  - [ ] `story help --all` output contains text from at least 30 distinct topics (the project has 35+ topics)
  - [ ] `cargo build` succeeds
  - [ ] `cargo test` passes with zero failures
- **Files modified**: `src/cli.rs` (parse_help function, Invocation enum), `src/app.rs` (HelpTopic/HelpCompact/HelpAll handler)
- **Estimated scope**: medium

---

### Wave 3 (depends on Wave 2)

#### T3.1: Rewrite session-start hook to inject CLI reference + project state
- **Depends on**: T2.1 (needs `story help --compact` to exist)
- **Requirement(s)**: R23, R28
- **Acceptance criteria**:
  - [ ] `plugin/claude-code/hooks/session-start.sh` calls `story help --compact` when `.storyhook/` exists
  - [ ] The hook still calls `story summary --json` and `story next --count 1 --json` for project state
  - [ ] The hook outputs valid JSON with a `systemMessage` key when `.storyhook/` exists
  - [ ] The `systemMessage` value contains CLI reference content (includes the string `story new` from compact help)
  - [ ] The `systemMessage` value contains project state (story counts, next story)
  - [ ] The hook outputs `{}` when `.storyhook/` does not exist
  - [ ] The hook outputs `{}` when plugin config has `enabled = "false"`
  - [ ] The hook still reads `cwd` from stdin JSON and changes directory
  - [ ] The hook does not reference MCP anywhere
  - [ ] The hook handles the case where `story` binary is not installed (outputs a helpful systemMessage)
  - [ ] The systemMessage is structured with clear section headers (e.g., `## CLI Reference`, `## Project State`)
- **Files modified**: `plugin/claude-code/hooks/session-start.sh`
- **Estimated scope**: medium

#### T3.2: Update scaffold templates for CLI-first guidance
- **Depends on**: T1.1 (MCP references removed from templates)
- **Requirement(s)**: R24
- **Acceptance criteria**:
  - [ ] `generate_storyhook_claude_md()` in `src/app.rs` produces a template that does NOT mention MCP
  - [ ] The template mentions `story help --compact` or `story help <topic>` as the way to learn commands
  - [ ] The template still includes the quick-reference command table
  - [ ] The template still includes the `.storyhook/` version control reminder
  - [ ] `generate_cursor_rules()` does not mention MCP (already handled in T1.1, but verify)
  - [ ] `cargo build` succeeds
  - [ ] `cargo test` passes
- **Files modified**: `src/app.rs`
- **Estimated scope**: small

**Note:** T3.2 technically only depends on T1.1 (not T2.1), but is placed in Wave 3 because it is a small task and benefits from the help system being complete -- the template can reference `story help --compact` which exists by this point.

---

## Requirement Traceability

| Requirement | Tasks | Coverage |
|-------------|-------|---------|
| R1: Delete src/mcp.rs | T1.1 | full |
| R2: Delete src/mcp_install.rs | T1.1 | full |
| R3: Delete MCP test files | T1.1 | full |
| R4: Remove mcp mods from lib.rs | T1.1 | full |
| R5: Remove --mcp flag from main.rs | T1.1 | full |
| R6: Remove McpConfig interactive handler from main.rs | T1.1 | full |
| R7: Remove McpConfig variant and parser from cli.rs | T1.1 | full |
| R8: Remove mcp-config from HELP_TEXT | T1.1 | full |
| R9: Remove mcp-config dispatch entry | T1.1 | full |
| R10: Remove McpConfig handler from app.rs | T1.1 | full |
| R11: Remove resolve_binary_path from app.rs | T1.1 | full |
| R12: Remove MCP from storyhook CLAUDE.md scaffold | T1.1 | full |
| R13: Remove MCP from cursor-rules scaffold | T1.1 | full |
| R14: Remove mcp-config help topic | T1.1 | full |
| R15: Remove MCP from json-format topic | T1.1 | full |
| R16: Update json-format test to not assert MCP | T1.1 | full |
| R17: Remove MCP from README.md | T1.2 | full |
| R18: Remove MCP from install.sh | T1.2 | full |
| R19: Remove mcp-config from cli-reference.md | T1.2 | full |
| R20: Remove MCP tests from story_types.rs | T1.1 | full |
| R21: story help --compact | T2.1 | full |
| R22: story help --all | T2.1 | full |
| R23: Rewrite session-start hook | T3.1 | full |
| R24: Update scaffold templates | T3.2 | full |
| R25: Compilation succeeds per wave | T1.1, T2.1, T3.1, T3.2 | full |
| R26: All tests pass | T1.1, T2.1, T3.1, T3.2 | full |
| R27: dialoguer stays | T1.1 | full |
| R28: Hook outputs valid systemMessage JSON | T3.1 | full |

No gaps.

---

## Risks

| Risk | Impact | Mitigation |
|------|--------|-----------|
| MCP references exist in files not identified in the goal | Compilation failure or dangling references | Grep entire `src/` for `(?i)mcp` after T1.1; grep tests/ similarly. The analysis above found `tests/story_types.rs` MCP tests not in the original file list -- they are now tracked in R20/T1.1. |
| `resolve_binary_path()` in `app.rs` has other callers added since last check | Compilation failure if deleted | Verified: only one call site (McpConfig handler). Safe to remove. |
| `story help --compact` output is too large for LLM context | Degrades hook usefulness | Target under 150 lines. The compact output should be a curated summary, not a dump. |
| Session-start hook Python dependency (uses `python3` for JSON parsing) | Breaks on systems without Python3 | Consider replacing Python JSON parsing with `jq` or pure bash, but this is existing behavior and out of scope unless user requests it. |
| Worktree copies (`.worktrees/`, `.claude/worktrees/`) contain stale MCP files | No build impact (not compiled), but confusing | Out of scope. Worktrees are independent checkouts. Flag for future cleanup. |

---

## Scope Boundaries

### IN scope
- Deleting all MCP server code, MCP installer code, MCP tests
- Removing all MCP references from Rust source, help topics, scaffold templates
- Removing MCP from README.md, install.sh, cli-reference.md
- Adding `story help --compact` and `story help --all`
- Rewriting the session-start hook to inject CLI docs + project state
- Updating scaffold templates to remove MCP and reference the new help system

### OUT of scope
- Cleaning up worktree copies of MCP files (`.worktrees/`, `.claude/worktrees/`)
- Replacing Python3 dependency in session-start hook with jq/bash
- Adding new help topics beyond what already exists
- Changing the `dialoguer` dependency or GitHub sync integration
- Modifying the TUI
- Version bump (suggest after all waves complete)
- Any Cargo.toml dependency cleanup (MCP code did not add unique dependencies beyond what the rest of the project uses)

---

## Resumption State

**Current state:** Plan created, no tasks started.

### Execution order for agents:
1. Execute T1.1 and T1.2 in parallel (or sequentially -- T1.2 is small and has no Rust impact)
2. After both complete: verify `cargo build && cargo test` passes
3. Execute T2.1
4. After T2.1: verify `cargo build && cargo test` passes, verify `story help --compact` and `story help --all` produce expected output
5. Execute T3.1 and T3.2 in parallel (or sequentially)
6. After all complete: full verification pass, suggest version bump

### Interruption recovery:
- If interrupted during T1.1: the partial state is dangerous (Rust won't compile with half-removed MCP). Either revert or complete all T1.1 changes before stopping.
- If interrupted after T1.1 but before T2.1: project compiles and tests pass. Safe stopping point. `story help --compact` does not exist yet but nothing depends on it externally.
- If interrupted after T2.1 but before T3.x: project compiles, help system works. Session-start hook still uses old behavior but functions. Safe stopping point.
- If interrupted during T3.1: hook may be in broken state. Either complete or revert the hook file.

---

## Deviation Log

| Task | Planned | Actual | Impact | Decision |
|------|---------|--------|--------|----------|
| (none yet) | | | | |
