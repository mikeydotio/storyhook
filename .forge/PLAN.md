# Implementation Plan — Fix Cycle 5

5 ESCALATE stories, all independent text/config changes. Single wave.

## Task Breakdown

### Wave 1 (all parallel — no dependencies)

- [ ] Task 1.1: Update HELP_TEXT to include --compact and --all flags (SH-34)
  - Acceptance: `src/cli.rs` HELP_TEXT contains `story help [<command>] [--compact] [--all]`; `cargo build` succeeds
  - Files: `src/cli.rs`

- [ ] Task 1.2: Remove ghost --tree reference from scaffold template (SH-35)
  - Acceptance: `story graph --tree` does not appear anywhere in `src/storage.rs`; remaining code block is well-formed with 2 examples; `cargo build` succeeds
  - Files: `src/storage.rs`

- [ ] Task 1.3: Sync Cargo.toml version + create post-bump hook (SH-36)
  - Acceptance:
    - `Cargo.toml` contains `version = "0.12.0"`
    - `.semver/hooks/post-bump/sync-cargo-toml.sh` exists, is executable (`chmod +x`), strips `v` prefix from `$NEW_VERSION` and updates Cargo.toml version field
    - `cargo build` succeeds
  - Files: `Cargo.toml`, `.semver/hooks/post-bump/sync-cargo-toml.sh`
  - **Note**: Semver plugin uses post-bump hooks, not a `tracked_files` config key. The user's original description said "add to config.yaml tracked files" but that field doesn't exist. The cycle-4 plan already researched this — hook is the correct approach.

- [ ] Task 1.4: Add CHANGELOG entry for MCP removal (SH-38)
  - Acceptance:
    - `CHANGELOG.md` has a `### Removed` section under `## [v0.12.0]`
    - Entry documents: MCP server removed, `--mcp` flag gone, `mcp-config` command gone
    - Entry documents replacement: session hooks via `story plugin install claude-code`
    - `### Removed` placed after `### Changed` and before `_[manual]_` marker
  - Files: `CHANGELOG.md`

- [ ] Task 1.5: Add CLI alternative to plugin install message (SH-39)
  - Acceptance: `src/plugin.rs` install success message contains both `/storyhook:context` and `story load-context`; `cargo build` succeeds
  - Files: `src/plugin.rs`

## Test Strategy

These are text/config changes — no new logic paths. Testing is:
1. `cargo build` — confirms no syntax/compile errors from string edits
2. `cargo test` — full existing suite passes (regression guard)
3. Evaluator grep checks — each acceptance criterion is verifiable by searching file contents

No new test files needed for this cycle. The version sync (SH-36) is guarded by the post-bump hook going forward.

## Resumption Points

Single wave — all 5 tasks are independent. If interrupted after any subset, the remaining tasks can be completed in any order. State is consistent after each individual task commit.

## Risk Register

| Risk | Impact | Mitigation |
|------|--------|------------|
| SH-36: post-bump hook has wrong sed syntax | Medium — Cargo.toml drifts on next bump | Hook uses simple `sed -i` on version line; test by inspecting script |
| SH-38: CHANGELOG wording misses a removed item | Low — incomplete docs | Acceptance criteria enumerate the specific items to mention |
| SH-35: removing line shifts line numbers | None — no other references to those lines | Single-line removal, self-contained |

## Scope Boundaries

**IN scope**: The 5 ESCALATE stories listed above and nothing else.
**OUT of scope**: New test files, types/epics feature work, any Rust logic changes.
