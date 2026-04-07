# Review Report

## Summary

The MCP removal and CLI-first documentation replacement is well-executed. All 6 stories are complete: MCP code is fully stripped from source, tests, docs, and scaffold templates. New features (`help --compact`, `help --all`, `session-start`, scaffold updates, hook rewrite) are functionally correct with strong test coverage. Three agents (reviewer, software-architect, devil's advocate) independently reviewed the codebase and converged on the same core findings.

## Findings

### UTF-8 Truncation Panic in session-start
- **Severity**: Critical
- **Description**: At `src/app.rs:2186`, `msg.truncate(3900)` is called when the systemMessage exceeds 3900 chars. `String::truncate()` in Rust panics if the index lands mid-codepoint. If story titles contain multi-byte UTF-8 characters (CJK, emoji) and the 3900th byte is mid-codepoint, `story session-start` will crash.
- **Location**: `src/app.rs:2186`
- **Option 1 (Recommended)**: Use `msg.floor_char_boundary(3900)` (stable since Rust 1.76, project requires 1.89). -- Pros: One-line fix, correct by construction. Cons: None.
- **Option 2**: Use `msg.char_indices().take_while(|(i, _)| *i < 3900).last()` to find the last valid boundary. -- Pros: Works on older Rust versions. Cons: More verbose.

### `--quiet` Flag Suppresses RawJson/session-start Output
- **Severity**: Important
- **Description**: In `render_response()` at `src/output.rs:117-119`, the `quiet` check runs before the `RawJson` bypass. If `story --quiet session-start` is invoked, output is silently suppressed instead of returning JSON. `RawJson` was designed to always output directly (per its comment on line 121), but `--quiet` overrides it.
- **Location**: `src/output.rs:117-119`
- **Option 1 (Recommended)**: Move the `RawJson` check before the `quiet` check in `render_response()`. -- Pros: Simple, consistent with `--json` bypass semantics. Cons: None meaningful.
- **Option 2**: Print directly to stdout in `session_start()`, bypassing `Response` entirely. -- Pros: Isolated change. Cons: Breaks output/render separation.

### Fragile TOML Parsing in Plugin-Config Check
- **Severity**: Important
- **Description**: `session_start()` at `src/app.rs:2127-2131` checks plugin-config.toml by lowercasing the entire file and using `contains("enabled")` + `contains("= false")`. False positives possible from: TOML comments containing "= false", keys like `autoenabled = false`, or `tracking = "false"`. Also fails on valid TOML with extra whitespace: `enabled  =   false`. All 5 review agents independently flagged this issue.
- **Location**: `src/app.rs:2127-2131`
- **Option 1 (Recommended)**: Parse with the `toml` crate (already a dependency) and check the `enabled` field properly. -- Pros: Robust, consistent with rest of codebase. Cons: Slightly more code.
- **Option 2**: Tighten to per-line matching with whitespace normalization. -- Pros: No struct needed. Cons: Still fragile for TOML edge cases.

### HELP_TEXT Missing --compact and --all Flags
- **Severity**: Important
- **Description**: `HELP_TEXT` at `src/cli.rs:78` shows `story help <command>` but omits `[--compact] [--all]`. Users running `story --help` or `story help` cannot discover the new LLM-optimized output modes.
- **Location**: `src/cli.rs:78`
- **Option 1 (Recommended)**: Update HELP_TEXT line to: `story help [<command>] [--compact] [--all]`. -- Pros: Standard CLI discoverability. Cons: None.
- **Option 2**: Add a note in the help topics footer. -- Pros: Less cluttered usage line. Cons: Easier to miss.

### Ghost Command `story graph --tree` in Init Template
- **Severity**: Important
- **Description**: `generate_claude_md()` in `src/storage.rs:262` references `story graph --tree {prefix}-1`. This flag does not exist. `parse_graph` only accepts `--critical-path`, `--blocked-by <id>`, and `--parallel-groups`. Pre-existing issue, but more impactful now that CLI docs are the sole integration surface.
- **Location**: `src/storage.rs:262`
- **Option 1 (Recommended)**: Replace with `story graph --blocked-by {prefix}-1` (closest existing equivalent). -- Pros: Accurate docs, no CLI change. Cons: Slightly different semantics.
- **Option 2**: Implement `--tree <id>` as a graph mode. -- Pros: Documented behavior works. Cons: YAGNI scope creep.

### VERSION File vs Cargo.toml Version Drift
- **Severity**: Important
- **Description**: VERSION file reads v0.12.0 but Cargo.toml declares `version = "0.6.0"`. The semver plugin bumps VERSION but Cargo.toml is not in sync. Pre-existing issue that grows worse with each release.
- **Location**: `VERSION` and `Cargo.toml:2`
- **Option 1 (Recommended)**: Add Cargo.toml to `.semver/config.yaml` as a tracked file. -- Pros: One-time fix, prevents future drift. Cons: Needs semver config update.
- **Option 2**: Manually sync Cargo.toml now. -- Pros: Immediate fix. Cons: Will drift again.

### compact_reference() Drift Risk
- **Severity**: Important
- **Description**: `compact_reference()` at `src/help_topics.rs:1142` is hand-curated. The CLI has 41 dispatch arms but compact reference intentionally omits some (member, state, type, epic, import-project). No mechanism detects when a new command is added without updating the reference — the exact discoverability problem this feature was built to solve.
- **Location**: `src/help_topics.rs:1142`
- **Option 1 (Recommended)**: Add an integration test that extracts dispatch arms from `parse_invocation` and asserts each appears in either `compact_reference()`, `HELP_TEXT`, or an explicit "intentionally excluded" list. -- Pros: Drift becomes a test failure. Cons: Test maintenance.
- **Option 2**: Accept the risk; rely on code review to catch drift. -- Pros: No test maintenance. Cons: Silent degradation over time.

### Compact Reference Tight Size Margin
- **Severity**: Useful
- **Description**: `story help --compact` output is 2966 bytes — only 34 chars (1.1%) under the 3000-char acceptance criterion. Adding one more command could exceed the limit. Tests will catch this, but the margin is tight.
- **Location**: `src/help_topics.rs:1142` (compact_reference content)
- **Option 1 (Recommended)**: No immediate action. The `help_compact_output_under_3000_chars` test guards against regression. -- Pros: Test is the safety net. Cons: None.
- **Option 2**: Shorten some descriptions to build margin. -- Pros: More headroom. Cons: Less helpful content.

### No CHANGELOG Entry for MCP Removal
- **Severity**: Useful
- **Description**: The MCP removal is a breaking change for anyone who configured `story --mcp` or `story mcp-config`. No CHANGELOG entry documents the removal or migration path. Historical CHANGELOG entries still reference MCP additions (acceptable as historical record).
- **Location**: `CHANGELOG.md`
- **Option 1 (Recommended)**: Add a version entry documenting MCP removal and that CLI-first via session hooks is the replacement. -- Pros: Clear migration guidance. Cons: Minor effort.
- **Option 2**: Document during next `semver bump`. -- Pros: Follows semver workflow. Cons: Gap until then.

### Stale Skill Invocation in plugin.rs Install Message
- **Severity**: Useful
- **Description**: `plugin.rs:107` outputs "Use /storyhook:context to get started" after plugin installation. While the skill exists, adding the CLI equivalent would be more consistent with the CLI-first direction.
- **Location**: `src/plugin.rs:107`
- **Option 1 (Recommended)**: Keep skill reference, add CLI alternative: "Use /storyhook:context to get started (or run 'story load-context' directly)." -- Pros: Covers both paths. Cons: Slightly longer.
- **Option 2**: Replace with CLI-only reference. -- Pros: Simpler. Cons: Misses installed skill.

### Two Other Hooks Still Use python3
- **Severity**: Useful
- **Description**: `post-git.sh:31` and `stop-handoff.sh:43` still use `python3 -c` for JSON parsing, creating an inconsistent dependency surface with the now-pure-bash session-start.sh.
- **Location**: `plugin/claude-code/hooks/post-git.sh:31`, `plugin/claude-code/hooks/stop-handoff.sh:43`
- **Option 1 (Recommended)**: Leave as-is. The other hooks do more complex JSON parsing that is harder to do in pure bash. python3 is a reasonable assumption in Claude Code environments. -- Pros: No churn. Cons: Inconsistent.
- **Option 2**: Migrate to CLI subcommands like session-start. -- Pros: Consistent architecture. Cons: Requires implementing new commands.

### sed-Based JSON Parsing in Hook
- **Severity**: Useful
- **Description**: `session-start.sh:15` uses `sed` to extract `cwd` from stdin JSON. This breaks on paths containing double-quote characters. Extremely unlikely in practice.
- **Location**: `plugin/claude-code/hooks/session-start.sh:15`
- **Option 1 (Recommended)**: Leave as-is — paths with double quotes are vanishingly rare. -- Pros: No change. Cons: Theoretical fragility.
- **Option 2**: Use `jq` with `sed` fallback. -- Pros: Robust. Cons: Adds optional dependency.

## Design Alignment

**ALIGNED**

The implementation matches the plan across all 6 stories:

| Story | Status | Notes |
|-------|--------|-------|
| SH-32: Strip MCP from Rust | ALIGNED | Zero MCP references in src/. All source files clean. |
| SH-33: Remove MCP from docs | ALIGNED | README, install.sh, cli-reference all clean. |
| SH-34: Help --compact/--all | ALIGNED | Implemented, tested, within size bounds. |
| SH-35: session-start command | ALIGNED | Correct JSON output, fallback behavior, size limits. |
| SH-36: Scaffold templates | ALIGNED | All three variants verified MCP-free. |
| SH-37: Hook rewrite | ALIGNED | 16 lines, no python3, delegates to CLI. |

Minor divergence: `session-start` intentionally omitted from HELP_TEXT since it's an internal/hook command. Reasonable design choice, not drift.

Module boundaries are clean: `help_topics.rs` owns help content, `cli.rs` owns parsing, `app.rs` owns dispatch, `output.rs` owns rendering. No leaking. `Response::RawJson` is a correct abstraction. No orphaned MCP dependencies in Cargo.toml.

## Strengths

- **Thorough MCP removal**: Zero MCP references in source code, plugin files, README, install.sh, or HELP_TEXT. `tests/mcp_removal.rs` has 14+ regression guards against reintroduction — gold standard for removal testing.
- **Session-start delegation pattern**: Moving all logic into `story session-start` and keeping the hook as a thin bash wrapper is the right architecture. The hook has no version-coupled logic.
- **Response::RawJson as explicit variant**: Cleaner than a boolean flag. Type system makes "no envelope" behavior visible. Defensive fallback arms in render paths prevent panics on refactor.
- **Test coverage depth**: 732 tests across 35+ files. New features have comprehensive behavioral tests covering happy paths, edge cases, special characters, performance bounds, and output contracts.
- **Compact reference quality**: Well-organized (LIFECYCLE, QUERY, METADATA, BULK, PROJECT), stays under 60 lines, actionable workflow tips. Good LLM context design.
- **Truncation guard**: The 3900-char truncation prevents session-start from blowing up the hook's output budget.
- **Zero mocks**: Entire test suite runs against the real compiled binary and real filesystem.
