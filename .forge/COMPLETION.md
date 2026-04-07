# Pipeline Complete

## Timestamp
2026-04-07T20:36:00Z

## Project
Replace MCP with CLI Documentation — storyhook v0.12.0

## Pipeline Summary
- Steps completed: interrogate → research → design → plan → decompose → execute → review → validate → triage → document → deploy
- Fix cycles: 5
- ESCALATE stories resolved: 14 (SH-12 through SH-15, SH-22 through SH-27, SH-34, SH-35, SH-36, SH-38, SH-39)
- Deployment: pushed to origin/main

## What Was Built
Removed the built-in MCP JSON-RPC server from storyhook and replaced it with a CLI-first integration model using session hooks and `story session-start`. Also added story types & epics feature with configurable type system, progress rollup, and full CLI/MCP tool support.

## Key Metrics
- Stories completed: 40 (SH-1 through SH-40)
- Tests: all passing (including 24 new validation tests + fix cycle tests)
- Release build: cargo build --release succeeds
- Documentation: CHANGELOG.md updated with ### Removed section for MCP removal

## Deviations from Original Idea
- MCP tools remain in the codebase for external MCP server support (storyhook_* tools) — only the built-in server process was removed
- Post-bump hook used instead of config.yaml tracked_files for Cargo.toml version sync (tracked_files doesn't exist in semver plugin)
- "Default" display for untyped stories instead of DESIGN.md's original "show default type from types.toml" approach

## Known Issues
- Pre-existing clippy warnings in src/github/field_map.rs (collapsible_if) — not introduced by this pipeline
- Some storyhook relationship inverse flags on stories (cosmetic, does not affect functionality)

## Post-Deployment Notes
- Cargo.toml version now synced to 0.12.0 — future semver bumps will auto-sync via post-bump hook
- Session hooks replace MCP for AI agent integration — users should run `story plugin install claude-code`
