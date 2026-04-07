# Project Documentation

## Overview

Storyhook is a local-first, CLI-first story and issue tracker built for developers and AI coding agents. It stores project data in a `.storyhook/` directory that travels with the repository, using append-only JSONL event logs for open stories and SQLite for archived (closed) stories.

The binary is called `story`. It provides short, pipeable commands with stable `--json` output and explicit exit codes, making it suitable for shell scripts, CI pipelines, and AI agent integrations. The project also ships a Claude Code plugin with session hooks and skill definitions.

**Current version:** v0.12.0 (VERSION file) / 0.6.0 (Cargo.toml -- see Known Issues for drift).

**Repository:** <https://github.com/mikeydotio/storyhook>

---

## Getting Started

### Install

**Quick install (Linux / macOS):**

```bash
curl -fsSL https://raw.githubusercontent.com/mikeydotio/storyhook/main/install.sh | sh
```

**With Cargo:**

```bash
cargo install storyhook
```

**From source:**

```bash
git clone https://github.com/mikeydotio/storyhook.git
cd storyhook
cargo install --path .
```

### Initialize a project

```bash
cd your-repo
story init                    # Creates .storyhook/ with default prefix "SH"
story init --prefix API       # Custom prefix: API-1, API-2, ...
```

This creates the `.storyhook/` directory containing `project.toml`, `states.toml`, `types.toml`, `members.jsonl`, `next-id`, and subdirectories for open stories and the archive database. It also generates:

- `.storyhook/CLAUDE.md` with workflow instructions for AI agents
- `AGENTS.md` at the project root for universal AI agent discoverability

### Create and work a story

```bash
story new "Implement user authentication"    # Creates SH-1
story show SH-1                              # View details
story move SH-1 in-progress                  # Transition state
story comment SH-1 "Added login endpoint"    # Add progress note
story move SH-1 done "Feature complete"      # Close (auto-archives)
```

### AI agent integration

Three commands support AI coding sessions:

- `story load-context` -- project overview with states, priorities, relationships, and ready work
- `story next` -- highest-priority unblocked story (respects dependencies)
- `story handoff --since 2h` -- session handoff summarizing recent changes

For Claude Code, install the plugin:

```bash
story plugin install claude-code
```

This copies the plugin to `~/.claude/plugins/storyhook/` and creates `.storyhook/plugin-config.toml`. The plugin provides session hooks (context injection, git sync, auto-handoff) and 7 skills accessible via `/storyhook:context`, `/storyhook:work`, etc.

---

## Architecture

### Event Sourcing Model

Storyhook uses event sourcing as its core data model. Each open story is a file (`SH-1.jsonl`) containing a sequence of events:

```jsonl
{"kind":"StoryCreated","at":"2026-04-01T12:00:00Z","title":"Build CLI","state":"todo"}
{"kind":"StoryAssigned","at":"2026-04-01T12:05:00Z","member_id":"mikey"}
{"kind":"StoryStateChanged","at":"2026-04-01T13:00:00Z","state":"in-progress"}
{"kind":"StoryCommentAdded","at":"2026-04-01T14:00:00Z","text":"Parser skeleton done"}
```

To read a story, the system replays (folds) all events into a `StorySnapshot` -- a materialized view of the current state. This fold is performed by `domain::fold_story()`.

**Event types** (from `domain.rs`):

| Event | Purpose |
|-------|---------|
| `StoryCreated` | Initial creation with title and starting state |
| `StoryCommentAdded` | Progress note or context |
| `StoryAssigned` | Assign to a team member |
| `StoryStateChanged` | Transition between states |
| `StoryAwaitingSet` | Mark as blocked with a reason |
| `StoryAwaitingCleared` | Remove blocker |
| `StoryRelationshipAdded` | Link to another story |
| `StoryRelationshipRemoved` | Remove a link |
| `StoryPrioritySet` | Set priority level |
| `StoryTypeSet` | Set story type (story, epic, bug, etc.) |
| `StoryLabelsSet` | Set labels |
| `StoryTitleSet` | Rename a story |
| `StoryClosedAndArchived` | Terminal event -- story moves to SQLite archive |
| `StoryDeleted` | Soft-delete with required reason |

### Module Overview

| Module | Responsibility |
|--------|---------------|
| `main.rs` | Entry point. Parses args, dispatches to TUI or CLI pipeline. |
| `cli.rs` | Command parsing. Splits global flags (`--json`, `--quiet`, `--no-hooks`), parses subcommands into an `Invocation` enum (40+ variants). No argument parsing library -- hand-rolled. |
| `app.rs` | Business logic. The `run()` function matches on `Invocation` and executes command handlers. Largest module (~2700 lines). |
| `domain.rs` | Core types: `StoryEvent`, `StorySnapshot`, `Priority`, `SuperState`, `StateDef`, `TypeDef`, `Member`. The `fold_story()` function, relationship validation, dependency graph analysis. |
| `storage.rs` | File I/O layer. JSONL read/write, TOML config loading/saving, SQLite archive operations, project initialization, `ProjectPaths` struct for path resolution. |
| `output.rs` | Display rendering. `Response` enum with variants (`Message`, `Story`, `Stories`, `Summary`, `Graph`, `RawJson`). `render_response()` handles human-readable and JSON output via a `JsonEnvelope`. |
| `help_topics.rs` | Extended help system. `BTreeMap` of topic names to help text. `compact_reference()` produces an LLM-optimized summary under 3000 chars. `all_topics_text()` concatenates all topics. |
| `plugin.rs` | Claude Code plugin install/uninstall. Copies bundled plugin from `plugin/claude-code/` to `~/.claude/plugins/storyhook/`. |
| `lock.rs` | Project-scoped file locking. `with_project_lock()` acquires an exclusive lock on `.storyhook/lock` with a 5-second timeout and 50ms poll interval. |
| `event_hooks.rs` | Event hook system. Loads `hooks-config.toml`, fires shell commands on story events (create, state change, close, etc.) with configurable timeouts. |
| `hooks.rs` | Git hook management. Installs/uninstalls post-commit, post-merge, and prepare-commit-msg hooks into `.git/hooks/`. |
| `decompose.rs` | Spec decomposition. Parses markdown or YAML spec files into `ImportStory` structs with relationships, supporting wave-based decomposition (`### Wave N` headings). |
| `tui.rs` | Terminal UI module. Dashboard, board view, story detail modal, inline editing, filter bar. Built on `ratatui` + `crossterm`. |
| `github.rs` | GitHub Issues sync (behind `github-sync` feature flag). Bidirectional sync with three-way merge conflict detection. Uses `ureq` (synchronous HTTP). |
| `error.rs` | Error types. `AppError` enum with variants mapping to exit codes 2-8. |

### Data Flow

```
User input
    |
    v
main.rs --> cli::split_global_flags() --> cli::parse_invocation()
    |                                            |
    v                                            v
CliOptions { json, quiet, no_hooks, invocation: Invocation }
    |
    v
app::run(root, options)
    |
    +-- lock::with_project_lock(root, || { ... })  (for write operations)
    |       |
    |       v
    |   storage::* functions  (read/write JSONL, TOML, SQLite)
    |       |
    |       v
    |   domain::fold_story()  (reconstruct snapshot from events)
    |
    v
output::Response
    |
    v
output::render_response(response, json, quiet)
    |
    v
stdout
```

### Storage Layout

```
.storyhook/
  project.toml          # Schema version, created_at, optional prefix, sync/doctor config
  states.toml           # Workflow states with OPEN/CLOSED superstate mapping
  types.toml            # Story types (story, epic, bug, chore, task)
  members.jsonl         # Team members (one JSON object per line)
  next-id               # Monotonic counter for story ID generation
  lock                  # Exclusive file lock (gitignored)
  plugin-config.toml    # Plugin settings (enabled, tracking verbosity)
  hooks-config.toml     # Event hook definitions (optional)
  CLAUDE.md             # AI agent instructions (auto-generated)
  .gitignore            # Excludes lock, WAL/SHM files
  open/
    stories/
      SH-1.jsonl        # Event log for open story SH-1
      SH-2.jsonl
    indexes/            # (Reserved for future use)
  archive/
    archive.db          # SQLite database for closed stories
    archive.db-wal      # WAL file (gitignored)
    archive.db-shm      # Shared memory file (gitignored)
```

---

## CLI Reference

The `story` binary uses a verb-first grammar (since v0.11.0). Run `story help --compact` for an LLM-optimized reference or `story help --all` for comprehensive documentation of every command.

### Core Commands

| Command | Purpose |
|---------|---------|
| `story init [--prefix P]` | Initialize project in current directory |
| `story new "<title>" [--state S] [--type T]` | Create a story |
| `story show <id>` | View story details |
| `story comment <id> "<text>"` | Add a comment |
| `story move <id> <state> ["comment"]` | Transition state |
| `story assign <id> <member>` | Assign to a member |
| `story block <id> "<reason>"` | Mark as blocked |
| `story unblock <id>` | Clear blocked status |
| `story prioritize <id> <level>` | Set priority (critical/high/medium/low/none) |
| `story label <id> <csv>` | Add labels |
| `story unlabel <id> <csv>` | Remove labels |
| `story delete <id> "<reason>"` | Soft-delete with audit trail |
| `story reopen <id>` | Reopen a closed story |
| `story set <id> [--field val ...]` | Batch update multiple fields |
| `story relate <a> <rel> <b>` | Add relationship |
| `story unrelate <a> <rel> <b>` | Remove relationship |

### Query Commands

| Command | Purpose |
|---------|---------|
| `story list [filters]` | List open stories with optional filters |
| `story next [--count N]` | Highest-priority ready story |
| `story search <query>` | Full-text search |
| `story summary` | State/priority breakdown |
| `story graph [mode]` | Dependency graph analysis |

### Project Management

| Command | Purpose |
|---------|---------|
| `story load-context [--format F]` | Project overview for AI sessions |
| `story handoff [--since D]` | Session handoff document |
| `story decompose <file> [--dry-run]` | Create stories from spec |
| `story import / export` | Bulk JSON import/export |
| `story doctor [--fix]` | Integrity checks and repair |
| `story phase list/show/add/remove/create` | Phase management |
| `story type list/add/remove` | Story type management |
| `story epic list/show/create/add` | Epic sugar commands |

### Integration Commands

| Command | Purpose |
|---------|---------|
| `story plugin install/uninstall claude-code` | Manage Claude Code plugin |
| `story scaffold agents-md/claude-md/cursor-rules` | Generate AI tool templates |
| `story hooks install/uninstall/list/test` | Git hook management |
| `story commit-sync [--since D]` | Sync git history with stories |
| `story github-sync [<id>] [--dry-run]` | Bidirectional GitHub Issues sync |
| `story session-start` | JSON output for plugin hooks |
| `story tui` | Interactive terminal UI |
| `story help [topic] [--compact] [--all]` | Help system |

### Global Flags

| Flag | Effect |
|------|--------|
| `--json` | Structured JSON output envelope |
| `--quiet` | Suppress normal success output |
| `--no-hooks` | Suppress event hook execution |

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 2 | Usage or validation error |
| 3 | Not found |
| 4 | Lock timeout |
| 5 | Integrity or storage error |
| 6 | GitHub auth error |
| 7 | GitHub API error |
| 8 | Sync conflict |

---

## Plugin System

### Claude Code Plugin

The plugin lives in `plugin/claude-code/` and is installed to `~/.claude/plugins/storyhook/` via `story plugin install claude-code`.

**Structure:**

```
plugin/claude-code/
  .claude-plugin/
    plugin.json           # Plugin metadata (name, version, author)
  hooks/
    hooks.json            # Hook event bindings
    session-start.sh      # SessionStart: injects CLI reference + project state
    post-git.sh           # PostToolUse (Bash): syncs git commits with stories
    stop-handoff.sh       # Stop: generates session handoff document
  skills/
    storyhook-setup/      # /storyhook:setup -- install and configure
    storyhook-context/    # /storyhook:context -- project overview
    storyhook-work/       # /storyhook:work -- start working on a story
    storyhook-plan/       # /storyhook:plan -- decompose specs into stories
    storyhook-handoff/    # /storyhook:handoff -- session handoff
    storyhook-triage/     # /storyhook:triage -- triage and cleanup
    storyhook-sync/       # /storyhook:sync -- git sync
  references/
    cli-reference.md      # Full CLI reference (available to skills)
    workflow-patterns.md  # Common workflow patterns
```

**Hooks:**

| Hook | Event | Behavior |
|------|-------|----------|
| `session-start.sh` | `SessionStart` | Runs `story session-start` to inject a systemMessage with CLI reference and project state. Pure bash, no python3 dependency. Falls back to `{}` on any failure. |
| `post-git.sh` | `PostToolUse` (Bash) | Detects git commit/merge/push in tool output. Runs `story commit-sync --since 1h` if storyhook project exists and git hooks are not already installed. |
| `stop-handoff.sh` | `Stop` | Generates `story handoff --since 4h` at session end. Outputs as systemMessage for the final response. |

**Plugin Configuration** (`.storyhook/plugin-config.toml`):

```toml
[plugin]
enabled = true
tracking = "normal"    # quiet | normal | verbose
```

Setting `enabled = false` causes `session-start` to return `{}`, effectively disabling all proactive behavior.

### Scaffold Templates

`story scaffold` generates instruction files for AI tools:

| Template | Output | Target Tool |
|----------|--------|-------------|
| `agents-md` | `AGENTS.md` | Any AI agent (universal format) |
| `claude-md` | `.storyhook/CLAUDE.md` content | Claude Code |
| `cursor-rules` | `.cursorrules` content | Cursor |

These templates contain CLI command tables, workflow instructions, and relationship reference. They are generated MCP-free and CLI-first as of the latest pipeline.

### Event Hooks

Separate from plugin hooks, storyhook supports event-driven hooks configured in `.storyhook/hooks-config.toml`:

```toml
[settings]
timeout_seconds = 10
enabled = true

[on_create]
command = "echo 'Story created: $STORY_ID'"

[on_state_change]
command = "notify-team.sh"

[on_close]
command = "cleanup.sh"
```

Supported events: `create`, `state_change`, `close`, `comment`, `priority_change`, `label_change`, `relationship_change`.

Hooks receive a JSON payload on stdin containing event details. They are suppressed with `--no-hooks`.

---

## Configuration

### project.toml

Created by `story init`. Contains project metadata.

```toml
schema = 1
created_at = "2026-04-01T00:00:00Z"
prefix = "SH"                        # Optional custom ID prefix

[sync]
auto_transition = true               # Optional: auto-transition on git sync

[doctor]
stale_threshold = "7d"               # Optional: threshold for stale story warnings
```

### states.toml

Defines workflow states. Each state maps to a superstate (`OPEN` or `CLOSED`).

```toml
[[states]]
slug = "todo"
super = "OPEN"

[[states]]
slug = "in-progress"
super = "OPEN"
role = "active"                      # Marks as the "working" state

[[states]]
slug = "done"
super = "CLOSED"
```

Rules:
- Must have at least one OPEN and one CLOSED state
- Moving a story to a CLOSED state immediately archives it to SQLite
- New projects default to: `todo` (OPEN), `in-progress` (OPEN, active), `done` (CLOSED)

### types.toml

Defines story types. Auto-created with defaults on first use.

```toml
[[types]]
slug = "story"
description = "A user story or feature"

[[types]]
slug = "epic"
description = "A large initiative containing child stories"

[[types]]
slug = "bug"
description = "A defect or regression"

[[types]]
slug = "chore"
description = "Maintenance or infrastructure work"

[[types]]
slug = "task"
description = "A discrete unit of work"
```

The first entry is the default type. Stories without an explicit type display as this type. The slug `none` is reserved.

### plugin-config.toml

Controls Claude Code plugin behavior.

```toml
[plugin]
enabled = true
tracking = "normal"
```

| Key | Values | Effect |
|-----|--------|--------|
| `enabled` | `true` / `false` | Enables/disables plugin hooks |
| `tracking` | `quiet` / `normal` / `verbose` | Controls auto-comment verbosity in skills |

### hooks-config.toml

Configures event-driven shell hooks (see Event Hooks section above).

### members.jsonl

One JSON object per line. Created by `story member add`.

```jsonl
{"id":"mikey","display_name":"Mikey Ward","email":"mw@mikey.io","github":null,"created_at":"2026-04-01T00:00:00Z"}
{"id":"octocat","display_name":"octocat","email":null,"github":"octocat","created_at":"2026-04-01T00:00:00Z"}
```

---

## Development

### Prerequisites

- Rust 1.89+ (edition 2024)
- Standard Unix tools for hook tests (bash, sed, grep)

### Building

```bash
cargo build                           # Debug build
cargo build --release                 # Release build
cargo build --no-default-features     # Without GitHub sync (drops ureq dependency)
```

### Testing

```bash
cargo test                            # Run all 732 tests
cargo fmt -- --check                  # Check formatting
cargo clippy --all-targets -- -D warnings   # Lint
```

**Test philosophy:** Zero mocks. Every test uses the real `story` binary (via `assert_cmd::Command::cargo_bin("story")`) and a real temporary filesystem (via `tempfile::tempdir()`). Hook tests execute real bash scripts.

**Test files** (34 files in `tests/`):

| File | Coverage Area |
|------|--------------|
| `story_flow.rs` | End-to-end story lifecycle |
| `cli_grammar.rs` | Verb-first command parsing (22 tests) |
| `relationships.rs` | All relationship types, inverses, cycles |
| `story_types.rs` | Type management, epic commands |
| `session_start.rs` | session-start JSON output, edge cases |
| `mcp_removal.rs` | Guards against MCP reintroduction |
| `help_new_flags.rs` | --compact and --all help modes |
| `session_start_hook.rs` | Hook script contracts (size, no python3) |
| `scaffold.rs` | Scaffold template generation |
| `tui_integration.rs` | TUI integration tests |
| `tui_undo.rs` | TUI undo/redo functionality |
| `doctor.rs` | Integrity checks and repair |
| `story_graph.rs` | Dependency graph analysis |

### Project Structure

```
storyhook/
  src/
    main.rs             # Entry point
    lib.rs              # Module declarations
    app.rs              # Command handlers (~2700 lines)
    cli.rs              # Argument parsing (~600 lines)
    domain.rs           # Core types and event fold (~500 lines)
    storage.rs          # File I/O and persistence (~500 lines)
    output.rs           # Display rendering
    help_topics.rs      # Help system content
    plugin.rs           # Plugin install/uninstall
    lock.rs             # File locking
    event_hooks.rs      # Event hook execution
    hooks.rs            # Git hook management
    decompose.rs        # Spec parsing
    tui.rs              # Terminal UI (module)
    error.rs            # Error types
    github.rs           # GitHub sync (feature-gated)
  tests/                # 34 integration test files
  plugin/
    claude-code/        # Claude Code plugin bundle
  install.sh            # Binary installer script
  VERSION               # Semver-managed version
  Cargo.toml            # Rust package manifest
  CHANGELOG.md          # Curated changelog
  AGENTS.md             # AI agent instructions
```

### Feature Flags

| Flag | Default | Effect |
|------|---------|--------|
| `github-sync` | On | Enables `story github-sync` and the `ureq` HTTP dependency |

Build without it: `cargo build --no-default-features`

### Key Dependencies

| Crate | Purpose |
|-------|---------|
| `serde` + `serde_json` | JSON serialization (events, snapshots, API output) |
| `toml` | TOML config file parsing |
| `chrono` | Timestamp generation (RFC 3339) |
| `rusqlite` (bundled) | SQLite archive database |
| `fs4` | Cross-platform file locking |
| `ratatui` + `crossterm` | Terminal UI |
| `ureq` (optional) | HTTP client for GitHub API |
| `thiserror` | Error type derivation |
| `assert_cmd` + `tempfile` (dev) | Integration testing |

---

## Architecture Decision Records

### ADR-001: CLI-First Over MCP

**Status:** Accepted (implemented in v0.12.0 pipeline)

**Context:** Storyhook originally provided an MCP (Model Context Protocol) server for AI tool integration, alongside the CLI. This created two parallel code paths for the same functionality. The MCP server added complexity (JSON-RPC framing, tool schema maintenance, a separate binary mode) while the CLI already provided everything agents needed via `--json` output and stable exit codes. Claude Code's plugin system supports shell command hooks natively, making MCP an unnecessary indirection layer.

**Decision:** Remove the MCP server entirely. Replace it with:
1. `story help --compact` / `story help --all` for LLM-consumable CLI documentation
2. `story session-start` CLI command that outputs session context JSON
3. A pure-bash session-start hook that delegates to `story session-start`
4. CLI-first scaffold templates (no MCP references)

**Consequences:**
- Simpler codebase: `mcp.rs`, `mcp_install.rs`, and associated tests deleted
- Single code path: all functionality flows through the CLI
- Easier testing: no JSON-RPC protocol to test; all tests use the real binary
- Plugin hooks are pure bash, no python3 dependency for session-start
- Breaking change for any users who relied on `story --mcp` or `story mcp-config`
- AI agents interact exclusively through shell commands and `--json` output

### ADR-002: Event Sourcing with JSONL

**Status:** Accepted (foundational design, v0.1.0+)

**Context:** Story trackers need to record changes over time. Options considered: (a) mutable state files (overwrite on each change), (b) a database (SQLite for everything), (c) append-only event logs.

**Decision:** Store open stories as append-only JSONL event streams. Each story is a single `.jsonl` file. The current state is reconstructed by folding (replaying) all events. Closed stories are archived into SQLite for compact storage.

**Consequences:**
- Full audit trail: every change is preserved with timestamps, never overwritten
- Simple writes: appending a line to a file is atomic and fast
- Git-friendly: JSONL files diff cleanly and merge manually if needed
- Concurrent safety: combined with file locking, append-only writes prevent corruption
- Read cost: every `show` or `list` replays all events for each story. Acceptable for typical project sizes (hundreds of stories, not millions)
- No query engine for open stories: filtering requires loading all snapshots into memory. Again, acceptable at expected scale
- Closed stories move to SQLite for space efficiency and to keep the open story directory small

### ADR-003: Zero-Mock Test Strategy

**Status:** Accepted (enforced since v0.1.0)

**Context:** The test suite needs to verify that the CLI works correctly end-to-end. Unit tests with mocked storage or domain logic would miss integration bugs (argument parsing edge cases, file permission issues, lock contention, JSON output formatting).

**Decision:** All tests use the real compiled binary via `assert_cmd::Command::cargo_bin("story")` and real temporary directories via `tempfile::tempdir()`. No mocks, no in-process testing of `app::run()`, no fake filesystems.

**Consequences:**
- High confidence: tests exercise the exact code path a user would hit
- Catches integration bugs: argument parsing, file I/O, lock behavior, JSON serialization
- Slower than unit tests: each test spawns a process and creates temp files (~7 seconds for 732 tests, which is acceptable)
- Harder to test internal edge cases: some domain logic is only reachable through specific CLI sequences
- Tests are more verbose: setting up a project requires `story init` + `story new` + ... for each test
- Real filesystem means tests are OS-dependent (Unix-only features like file permissions)

---

## Known Issues

Nine items escalated from the triage process. Listed by severity.

### Critical

**SH-31: UTF-8 Truncation Panic** -- `msg.truncate(3900)` at `src/app.rs:2186` panics if the truncation point lands mid-codepoint in a multi-byte UTF-8 string. Projects with CJK or emoji characters in story titles that push the systemMessage past 3900 bytes will crash the binary.
- **Fix:** `msg.truncate(msg.floor_char_boundary(3900))` (one-line fix, stable since Rust 1.76).

### High

**SH-32: Fragile TOML Parsing in Plugin Config** -- `session_start()` at `src/app.rs:2129` uses `contains("= false")` to check `plugin-config.toml`. Fails on valid TOML with extra whitespace (e.g., `enabled  =   false`). False positives possible from comments.
- **Fix:** Parse with the `toml` crate (already a dependency) into a proper struct.

**SH-33: --quiet Suppresses RawJson/session-start** -- In `render_response()` at `src/output.rs`, the `quiet` check runs before the `RawJson` bypass. Running `story --quiet session-start` silently returns empty instead of the expected JSON.
- **Fix:** Move the `RawJson` check before the `quiet` check (3-line swap).

### Medium

**SH-34: HELP_TEXT Missing --compact and --all Flags** -- `HELP_TEXT` at `src/cli.rs:78` shows `story help <command>` but omits `[--compact] [--all]`. Users cannot discover the LLM-optimized output modes from the main help text.

**SH-35: Ghost --tree Command in Scaffold Template** -- `generate_claude_md()` at `src/storage.rs:262` references non-existent `story graph --tree`. This command does not exist. The line should be removed or replaced with a valid flag.

**SH-36: VERSION File vs Cargo.toml Drift** -- `VERSION` is `v0.12.0` but `Cargo.toml` is `0.6.0`. The semver plugin bumps VERSION but Cargo.toml is not tracked in `.semver/config.yaml`.
- **Fix:** Add Cargo.toml to semver config and sync the versions.

**SH-38: No CHANGELOG Entry for MCP Removal** -- The MCP removal is a breaking change with no CHANGELOG entry documenting the removal, the replacement approach, or migration path.

### Low

**SH-37: compact_reference() Drift Risk** -- The hand-curated `compact_reference()` function has no test to detect when new commands are added but not reflected in the compact output.

**SH-39: Stale Skill Invocation in Plugin Install Message** -- `plugin.rs:107` mentions `/storyhook:context` skill but not the CLI equivalent, inconsistent with the CLI-first direction.

---

## Test Coverage

**Total:** 732 tests, all passing. Zero failures, zero skipped.
**Duration:** ~7 seconds.
**Build:** Clean (no errors). One pre-existing clippy warning (cosmetic `collapsible_if` in `cli.rs:311`).

### Coverage by Area

| Area | Test Files | Key Assertions |
|------|-----------|----------------|
| Story lifecycle | `story_flow.rs`, `story_state_archive.rs`, `story_reopen.rs` | Create, transition, close, archive, reopen |
| CLI grammar | `cli_grammar.rs` (22 tests) | All verb-first commands parse and execute correctly |
| Relationships | `relationships.rs` | All 15 relationship types, inverse creation, cycle detection |
| Story types & epics | `story_types.rs` | Type CRUD, epic sugar, progress rollup |
| Filtering | `story_list_filters.rs`, `story_list_flagged.rs` | State, assignee, priority, label, blocked, ready, stale filters |
| Dependencies | `story_graph.rs` | Critical path, blocked chains, parallel groups |
| Help system | `cli_help.rs`, `help_new_flags.rs` | Topic lookup, --compact size/content, --all completeness |
| Session start | `session_start.rs` (15+ tests) | JSON shape, edge cases, special characters, performance |
| MCP removal | `mcp_removal.rs` | Guards against MCP file/code reintroduction |
| Hook scripts | `session_start_hook.rs` | Script size, no python3, delegates to CLI |
| Scaffolds | `scaffold.rs` | Template generation, no MCP references |
| Decompose | `story_decompose.rs` | Markdown/YAML parsing, wave syntax, relationships |
| TUI | `tui_integration.rs`, `tui_undo.rs` | Board view, undo/redo, 245 total TUI tests |
| Doctor | `doctor.rs` | Integrity checks, auto-repair |
| Import/Export | `story_import.rs`, `story_export.rs` | Bulk operations, round-trip |
| Search | `story_search.rs` | Full-text search across titles and comments |
| GitHub sync | (covered by `story_sync_git.rs`) | Commit-sync integration |

### Contract Tests

The test suite enforces several invariants:
- `compact_reference()` output is under 3000 characters and between 40-100 lines
- `session-start` output is one of exactly two valid shapes: `{}` or `{"systemMessage":"..."}`
- `session-start` completes within 2 seconds
- Session-start hook script is under 20 functional lines and contains no python3
- No source file contains MCP-related code (regression guard)

---

## Upcoming / Planned

The **Story Types & Epics** feature (documented in `.forge/DESIGN.md` and `.forge/IDEA.md`) adds:
- Configurable story types via `types.toml` (story, epic, bug, chore, task)
- `story type list/add/remove` commands
- `story epic list/show/create/add` sugar commands (delegating to existing primitives)
- Progress rollup for parent stories (children done / children total)
- `--type` filter on `story list` and `story next`
- Type validation at write time, doctor integrity checks for unknown types

This feature is partially implemented in the codebase (CLI parsing and domain types exist) but is not yet fully wired or tested. See DESIGN.md for the complete architecture specification.
