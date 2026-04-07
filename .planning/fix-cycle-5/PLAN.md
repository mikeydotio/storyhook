# Fix Cycle 5 -- Implementation Plan

**Cycle**: 5 (ESCALATE stories from review)
**Date**: 2026-04-07
**Version**: v0.12.0

---

## Requirements

| ID | Requirement | Story | Type | Priority |
|----|-------------|-------|------|----------|
| R1 | HELP_TEXT includes --compact and --all flags on the help line | SH-34 | fix | high |
| R2 | Scaffold CLAUDE.md template does not reference nonexistent --tree flag | SH-35 | fix | high |
| R3 | Cargo.toml version matches VERSION file (0.12.0) and is tracked by semver | SH-36 | fix | high |
| R4 | CHANGELOG documents MCP server removal (breaking change from a prior release) | SH-38 | fix | medium |
| R5 | Plugin install message references `story load-context` instead of stale skill invocation | SH-39 | fix | high |

---

## Task Waves

### Wave 1 (parallel -- no dependencies, all independent)

#### T1.1: Fix HELP_TEXT help line to include --compact and --all (SH-34)
- **Requirement(s)**: R1
- **Description**: In `src/cli.rs` line 78, change `story help <command>` to `story help [<command>] [--compact] [--all]`
- **Acceptance criteria**:
  - [ ] `src/cli.rs` contains the string `story help [<command>] [--compact] [--all]` in the HELP_TEXT constant
  - [ ] `src/cli.rs` does NOT contain the string `story help <command>` (the old form) in HELP_TEXT
  - [ ] `cargo build` succeeds with exit code 0
- **Expected files**: `src/cli.rs`
- **Estimated scope**: small

#### T1.2: Remove ghost --tree example from scaffold template (SH-35)
- **Requirement(s)**: R2
- **Description**: In `src/storage.rs` around line 262, remove the `story graph --tree {prefix}-1` example line from the `generate_claude_md()` template. The surrounding `story graph` and `story graph --blocked-by` lines remain.
- **Acceptance criteria**:
  - [ ] `src/storage.rs` does NOT contain the string `--tree` anywhere in the `generate_claude_md` function (or file, since it only appears there)
  - [ ] The `story graph` line and `story graph --blocked-by` line still exist in the template
  - [ ] `cargo build` succeeds with exit code 0
- **Expected files**: `src/storage.rs`
- **Estimated scope**: small

#### T1.3: Sync Cargo.toml version and add semver tracking (SH-36)
- **Requirement(s)**: R3
- **Description**: (1) Change `version = "0.6.0"` to `version = "0.12.0"` in `Cargo.toml` line 3. (2) Add a `tracked_files` entry for `Cargo.toml` in `.semver/config.yaml` so future semver bumps update it automatically.
- **Acceptance criteria**:
  - [ ] `Cargo.toml` line starting with `version =` reads `version = "0.12.0"`
  - [ ] `.semver/config.yaml` contains an entry that references `Cargo.toml` (the exact key depends on semver plugin format -- likely a `tracked_files` list or `files` key)
  - [ ] `cargo build` succeeds with exit code 0 (Cargo.toml is valid TOML)
  - [ ] The VERSION file still reads `v0.12.0` (unchanged)
- **Expected files**: `Cargo.toml`, `.semver/config.yaml`
- **Estimated scope**: small

#### T1.4: Add CHANGELOG entry for MCP removal (SH-38)
- **Requirement(s)**: R4
- **Description**: Add a retroactive entry to `CHANGELOG.md` documenting the MCP server removal. This was a breaking change that happened between v0.6.0 and v0.12.0. Insert an entry (or add to an existing version's section) that documents: MCP server removed, `--mcp` and `mcp-config` CLI flags gone, session hooks are the replacement, `story plugin install claude-code` sets up hooks.
- **Acceptance criteria**:
  - [ ] `CHANGELOG.md` contains the word `MCP` (case-sensitive) at least once in a removal/breaking-change context
  - [ ] `CHANGELOG.md` contains the strings `--mcp` and `mcp-config` documenting their removal
  - [ ] `CHANGELOG.md` contains the string `story plugin install claude-code` as the migration path
  - [ ] `CHANGELOG.md` mentions session hooks as the replacement mechanism
  - [ ] The file still begins with `# Changelog` (header preserved)
  - [ ] Existing changelog entries are not modified (only additions)
- **Expected files**: `CHANGELOG.md`
- **Estimated scope**: small

#### T1.5: Fix stale skill invocation in plugin install message (SH-39)
- **Requirement(s)**: R5
- **Description**: In `src/plugin.rs` lines 106-107, change the message from referencing `/storyhook:context` to include `(or run story load-context directly)`. The exact new text should replace or augment the stale skill reference.
- **Acceptance criteria**:
  - [ ] `src/plugin.rs` contains the string `story load-context` in the install success message
  - [ ] `src/plugin.rs` does NOT contain the string `/storyhook:context` (the stale skill invocation is removed or replaced)
  - [ ] `cargo build` succeeds with exit code 0
- **Expected files**: `src/plugin.rs`
- **Estimated scope**: small

---

## Requirement Traceability

| Requirement | Task(s) | Coverage |
|-------------|---------|----------|
| R1: HELP_TEXT flags | T1.1 | full |
| R2: Ghost --tree | T1.2 | full |
| R3: VERSION drift | T1.3 | full |
| R4: CHANGELOG MCP | T1.4 | full |
| R5: Stale skill msg | T1.5 | full |

No gaps. All requirements map to exactly one task each.

---

## Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| semver config.yaml format unknown -- tracked_files key may differ | T1.3 may use wrong key name | Executor should read `.semver/config.yaml` and any semver plugin docs/schema before editing |
| CHANGELOG entry placement -- unclear which version section the MCP removal belongs under | Entry placed in wrong version section | Executor should check git history for when MCP was removed and place entry in correct version block |
| Cargo.toml version change could affect `cargo install` or crate publishing | Minimal -- this is fixing drift, not introducing it | Verify `cargo build` passes |

---

## Scope Boundaries

**IN scope:**
- The 5 string/config changes described above (SH-34, SH-35, SH-36, SH-38, SH-39)
- One commit per story, conventional-commit style

**OUT of scope:**
- Running the full test suite (this is the executor's judgment call, not a plan requirement)
- Any Rust logic changes beyond string literals
- Version bumps (these are fixes within v0.12.0)
- Any new features, refactors, or test additions
- Updating AGENTS.md or other generated scaffolds (those are downstream of the template fix in T1.2, not part of this cycle)

---

## Resumption State

All 5 tasks are in Wave 1 with no dependencies. Execution can resume from any point by checking which files have been modified:

| Task | Resume check |
|------|-------------|
| T1.1 | `grep 'story help \[<command>\]' src/cli.rs` -- if found, done |
| T1.2 | `grep '\-\-tree' src/storage.rs` -- if NOT found, done |
| T1.3 | `grep 'version = "0.12.0"' Cargo.toml` -- if found, done; also check `.semver/config.yaml` for Cargo.toml reference |
| T1.4 | `grep 'MCP' CHANGELOG.md` -- if found in a removal context, done |
| T1.5 | `grep 'story load-context' src/plugin.rs` -- if found, done |

---

## Deviation Log

_Empty at plan creation. To be filled during execution._

| Task | Planned | Actual | Impact | Decision |
|------|---------|--------|--------|----------|
| -- | -- | -- | -- | -- |
