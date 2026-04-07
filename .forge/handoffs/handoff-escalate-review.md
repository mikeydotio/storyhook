# Handoff: ESCALATE Review → Plan (Fix Cycle 5)

## Summary
User reviewed all 5 ESCALATE stories from fix cycle 4. All recommended approaches were approved. These are text-only or config-only changes — no architectural risk.

## User Decisions

### SH-34 — HELP_TEXT missing --compact and --all flags
**Decision:** Update HELP_TEXT line
**Action:** Change `story help <command>` to `story help [<command>] [--compact] [--all]` at `src/cli.rs:78`

### SH-35 — Ghost command --tree in scaffold init template
**Decision:** Remove the line entirely (Skeptic recommendation)
**Action:** Remove the `story graph --tree {prefix}-1` example line from `generate_claude_md()` at `src/storage.rs:262`

### SH-36 — VERSION file vs Cargo.toml version drift
**Decision:** Add Cargo.toml to semver config + sync
**Action:** Add Cargo.toml to `.semver/config.yaml` tracked files AND set `Cargo.toml` version to `0.12.0`

### SH-38 — No CHANGELOG entry for MCP removal
**Decision:** Add CHANGELOG entry now
**Action:** Add version entry documenting: MCP server removed, `--mcp` and `mcp-config` gone, session hooks are the replacement, `story plugin install claude-code` sets up hooks

### SH-39 — Stale skill invocation in plugin install message
**Decision:** Add CLI alternative
**Action:** Change `src/plugin.rs:107` message to include `(or run story load-context directly)`

## Context for Planner
- All 5 stories are independent — no ordering constraints
- All are single-file, single-string or single-config changes
- SH-36 touches 2 files (Cargo.toml + .semver/config.yaml)
- SH-38 may need to create CHANGELOG.md if it doesn't exist
- Stories already exist in storyhook with acceptance criteria from triage — planner should use them directly
- Fix cycle 5 inherits from cycle 4: max_fix_cycles may need override since we're at cycle 4 of 3

## What's Next
Dispatch to `/forge plan --orchestrated` with these 5 stories as input.
