# Implementation Plan (FIX Cycle 4)

## Scope

8 ESCALATE stories promoted from FIX due to max fix cycles. All user-approved with specific fix decisions. SH-37 (compact_reference drift) was accepted as no-action and closed.

## Task Breakdown

### Wave 1 (all independent — no cross-task dependencies)

**Note:** T1.1 and T1.2 both modify `src/app.rs` — assign to the same generator to avoid merge conflicts.

---

#### T1.1: UTF-8 safe truncation (SH-31)
- **Description**: Replace `msg.truncate(3900)` with `msg.truncate(msg.floor_char_boundary(3900))` at `src/app.rs:2186`.
- **Acceptance**:
  - `msg.truncate(3900)` does not appear in `src/app.rs`
  - `floor_char_boundary` appears in the truncation block
  - New test: create stories with multi-byte UTF-8 titles (CJK/emoji) that push systemMessage past 3900 bytes. Assert: exit 0, valid JSON, systemMessage present
  - `cargo test` passes
- **Files**: `src/app.rs`, `tests/session_start.rs`

#### T1.2: Proper TOML parsing for plugin config (SH-32)
- **Description**: Replace `contains("= false")` string matching at `src/app.rs:2127-2131` with `toml` crate parsing.
- **CRITICAL**: Must handle BOTH config formats found in tests:
  - With section: `[plugin]\nenabled = false\n` (written by `story plugin install`)
  - Bare key: `enabled = "false"\n` (used in tests at `session_start.rs:60` and `session_start_hook.rs:127`)
  - Recommended struct: top-level `enabled` + optional `plugin.enabled`, check both
- **Must preserve fail-open behavior**: malformed/missing config → treat as enabled (existing test at `session_start.rs:649`)
- **Acceptance**:
  - `contains("= false")` and `contains("= \"false\"")` do not appear in `src/app.rs`
  - `toml::from_str` or `toml::de` appears in plugin config section
  - Existing test `session_start_plugin_config_extra_whitespace_bug_documented` updated: now asserts `{}` (fixed behavior)
  - New tests: (a) no-space `enabled=false`, (b) comments + extra keys, (c) `[plugin]` nested table
  - Existing tests for enabled=true, enabled=false, enabled="false", malformed all pass
  - `cargo test` passes
- **Files**: `src/app.rs`, `tests/session_start.rs`

#### T1.3: RawJson bypasses --quiet (SH-33)
- **Description**: In `render_response()` at `src/output.rs:117-119`, move the RawJson match arm before the `if quiet` early return.
- **Acceptance**:
  - In `render_response()`, `Response::RawJson` arm appears before the `if quiet` check
  - New test: `story --quiet session-start` on a valid project returns non-empty valid JSON with `systemMessage` key
  - `cargo test` passes
- **Files**: `src/output.rs`, `tests/session_start.rs`

#### T1.4: Fix HELP_TEXT (SH-34)
- **Description**: Update `story help <command>` to `story help [<command>] [--compact] [--all]` in HELP_TEXT at `src/cli.rs:78`.
- **Acceptance**:
  - `story help <command>` (without brackets) does not appear in HELP_TEXT
  - `--compact` and `--all` appear in the help line
  - `cargo build` succeeds
- **Files**: `src/cli.rs`

#### T1.5: Remove ghost --tree reference (SH-35)
- **Description**: Remove the line referencing `story graph --tree` at `src/storage.rs:262`. Do not replace.
- **Acceptance**:
  - `--tree` does not appear in `src/storage.rs`
  - `cargo test` passes
- **Files**: `src/storage.rs`

#### T1.6: Sync VERSION and Cargo.toml versions (SH-36)
- **Description**: Two changes:
  1. Update `Cargo.toml` version from `"0.6.0"` to `"0.12.0"` (matching VERSION file, no `v` prefix)
  2. Create post-bump hook at `.semver/hooks/post-bump/sync-cargo-toml.sh` that strips the `v` prefix from `$NEW_VERSION` and updates `Cargo.toml` version field, then `git add Cargo.toml && git commit --amend --no-edit`
- **Note**: Semver plugin uses post-bump hooks (not config keys) for tracking additional files.
- **Acceptance**:
  - `Cargo.toml` contains `version = "0.12.0"`
  - `.semver/hooks/post-bump/sync-cargo-toml.sh` exists, is executable, references `Cargo.toml`
  - `cargo build` succeeds
  - New test: VERSION and Cargo.toml versions match (after stripping `v` prefix)
- **Files**: `Cargo.toml`, `.semver/hooks/post-bump/sync-cargo-toml.sh`, `tests/version_sync.rs`

#### T1.7: Add CHANGELOG entry for MCP removal (SH-38)
- **Description**: Add a `### Removed` section under the v0.12.0 entry in `CHANGELOG.md` documenting MCP removal. Include: what was removed (`--mcp` flag, `mcp-config` command, all `storyhook_*` MCP tools), replacement (session hooks via `story plugin install claude-code` + `story load-context`), and migration path.
- **Acceptance**:
  - `CHANGELOG.md` contains `### Removed` under `## [v0.12.0]`
  - Entry mentions "MCP" and describes the removal
  - Entry references replacement: `story load-context` or `session hooks` or `story plugin install`
- **Files**: `CHANGELOG.md`

#### T1.8: Add CLI alternative to plugin install message (SH-39)
- **Description**: At `src/plugin.rs:106-107`, update the install success message to mention both the skill and CLI command. Change `"Use /storyhook:context to get started."` to include `story load-context`.
- **Acceptance**:
  - `src/plugin.rs` contains both `story load-context` and `storyhook:context` in the install message
  - `cargo build` succeeds
  - `cargo test` passes
- **Files**: `src/plugin.rs`

## Test Strategy

| Fix | New Tests | Updated Tests | Priority |
|-----|-----------|---------------|----------|
| SH-31 | 1 (multi-byte UTF-8 truncation) | — | Critical |
| SH-32 | 3 (no-space, comments, nested table) | 1 (whitespace bug → fixed) | High |
| SH-33 | 1 (--quiet with session-start) | — | High |
| SH-34 | — | — | — |
| SH-35 | — | — | — |
| SH-36 | 1 (version sync) | — | Medium |
| SH-38 | — | — | — |
| SH-39 | — | — | — |

Total: 6 new tests, 1 updated test.

## Risk Register

| Risk | Impact | Mitigation |
|------|--------|------------|
| SH-32 struct rejects bare-key configs | HIGH — breaks 2 existing tests | Plan specifies handling both formats; acceptance criteria require all existing tests pass |
| SH-32 TOML parse error changes fail-open to fail-closed | HIGH — silently disables plugin | Plan requires fail-open behavior preserved; existing malformed config test must pass |
| SH-31 post-truncation + JSON envelope exceeds downstream limit | LOW — 4000 limit is on deserialized value, not wire format | Existing test checks deserialized length; matches current behavior |
| SH-36 post-bump hook schema wrong | MEDIUM — hook doesn't fire | Hook follows documented pattern from semver plugin |
| T1.1 + T1.2 merge conflict in app.rs | MEDIUM | Assigned to same generator |

## Scope Boundaries

**IN scope**: The 8 fixes listed, their targeted tests, and nothing else.

**OUT of scope**: Bash hook TOML parsing (same class of bug as SH-32 but in shell scripts), recursive UTF-8 hardening beyond truncation point, compact_reference drift tests (SH-37 accepted), TUI changes.

## Resumption Points

After Wave 1 completes, state is consistent and the full test suite should pass. Single wave — no intermediate resumption needed.
