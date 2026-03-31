# Changelog

All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [v0.11.0] - 2026-03-31

### Changed
- **CLI grammar restructured to verb-first** — all story commands now use `story <verb> <id> [args]` instead of `story <id> <verb> [args]`. Old forms removed entirely.
- `story <id> is <state>` → `story move <id> <state>` — industry-standard verb for state transitions
- `story <id> awaits "<reason>"` → `story block <id> "<reason>"` — universally understood verb
- `story <id> awaits --clear` → `story unblock <id>` — symmetric pair with `block`
- `story <id> priority <level>` → `story prioritize <id> <level>` — verb form
- `story <id> label --remove <csv>` → `story unlabel <id> <csv>` — consistent `un-` prefix
- `story <a> <rel> <b> [--remove]` → `story relate <a> <rel> <b>` / `story unrelate <a> <rel> <b>`
- All help topics, AGENTS.md, CLAUDE.md template, cursor-rules template, and git hooks updated to verb-first syntax

### Added
- `story show <id>` — explicit verb for viewing stories (previously `story <id>`)
- `story comment <id> "<text>"` — explicit verb for adding comments (previously `story <id> "<text>"`)
- `story set <id> [--field value ...]` — batch update multiple fields in one command
- `story set <id> --json '{"key":"value"}'` — JSON mode for structured batch updates with field validation
- `story link` / `story unlink` — aliases for `story relate` / `story unrelate`
- 14 new help topics: show, move, block, unblock, set, comment, assign, prioritize, label, unlabel, relate, unrelate, reopen, delete
- Redirect aliases: `story help is` → move, `story help awaits` → block, `story help priority` → prioritize, `story help link` → relate
- 22 new integration tests in `tests/cli_grammar.rs` covering all verb-first commands

_[manual]_

## [v0.10.0] - 2026-03-31

### Added
- Default `in-progress` state (OPEN, role=active) — new projects now ship with todo/in-progress/done out of the box
- `--state <slug>` flag on `story new` to set initial state at creation time
- `state` parameter on `storyhook_create_story` and `storyhook_bulk_create` MCP tools for setting initial state
- `storyhook_bulk_update` MCP tool for batch state changes (bulk close, bulk reopen, bulk transitions)
- `storyhook_add_relationship` MCP tool with `{a, relation, b}` params and enum for all 8 relationship types
- `storyhook_delete_story` MCP tool and `story <id> delete "<reason>"` CLI — soft-delete with required reason, archived with deletion flag for full audit trail
- Nested checklist support in `story decompose` — indented `- [ ]` items create parent-child relationships to their parent checkbox
- Relationship summary in decompose response — shows created relationships after decomposition
- `state` field on `ImportStory` for setting initial state during bulk import

### Changed
- All MCP tool descriptions enriched with relationship type enums, available states, dependency hints (`blocks`/`blocked-by`), and cross-references to related tools
- `storyhook_decompose_spec` description now documents Wave syntax, nested checklists, and inline priority/label markers
- Archive database schema: added `deleted_reason` column for soft-delete audit trail

_[manual]_

## [v0.9.0] - 2026-03-31

### Added
- Two-way GitHub Issues sync via `story github-sync [<id>] [--dry-run]` — full bidirectional sync between storyhook stories and GitHub Issues with three-way merge conflict detection, interactive resolution, and per-story atomicity
- GitHub API client (`ureq` 3.x, synchronous, no tokio) behind `github-sync` cargo feature flag for optional builds without network dependencies
- Fenced `storyhook` YAML code block in GitHub Issue bodies for encoding non-native fields (priority, awaiting, non-native relationships)
- Native GitHub Sub-issues and Dependencies API integration (API version 2026-03-10) for `parent-of`/`child-of` and `blocks`/`blocked-by` relationship sync
- Initial sync setup wizard with import-all, title-match, push-only, and start-fresh strategies
- Configurable sync modes (`off`/`manual`/`auto`) — auto mode triggers per-story sync on any story-modifying command
- Sync state persistence in `.storyhook/github-sync.toml` with base snapshots for three-way merge and pre-sync backups for rollback
- `story github-sync --dry-run` to preview sync changes without applying them
- `storyhook_github_sync` MCP tool for AI agent integration

### Changed
- Renamed `story sync-git` to `story commit-sync` (old name kept as alias for backward compatibility)
- Renamed `storyhook_sync_git` MCP tool to `storyhook_commit_sync` (old name kept as alias)
- New error exit codes: 6 (GitHub auth), 7 (GitHub API), 8 (sync conflict)

_[manual]_

## [v0.8.0] - 2026-03-29

### Added
- TUI drag-and-drop: drag story rows to section headers to move between states (d564d12)
- TUI dependency graph view (`3` key) with Tree, Dependencies, Critical Path, and Focus modes (a82c01b)
- TUI session-only undo/redo via `Ctrl+Z` / `Ctrl+Y` with event snapshot/restore (b08edfb)
- Wave-based markdown format in `story decompose` — `### Wave N` headings auto-generate `follows` relationships between waves (4f9336f)
- `story help json-format` documenting the complete JSON output contract for programmatic consumers (09af4c2)

### Fixed
- `.storyhook/lock` and SQLite WAL/SHM files now gitignored via `.storyhook/.gitignore` created during `story init` (88a6736)

_[manual]_

## [v0.7.0] - 2026-03-29

### Added
- Full-featured terminal UI via `story tui` — dashboard home screen, grouped-table board view with collapsible state sections, story detail modal with inline editing, create form, persistent filter bar, help overlay, and Phase 1 mouse support (b9024ae, 8009ddc, 11079e4, f6fbca8, 1c44348, 7d8ddae)
- `StoryTitleSet` event variant in the domain layer, enabling title editing from the TUI (ba3461d)
- 245 tests covering the TUI (225 unit + 19 integration + 1 performance) (6f3bc0e)
- `story help tui` help topic documenting TUI keybindings and usage (6f3bc0e)

### Fixed
- Title editing in TUI now actually persists via `StoryTitleSet` event instead of writing a comment (ba3461d)

### Maintenance
- Track tool config files (.storyhook, .semver) and gitignore .planning directory (b682963)

_[manual]_

## [v0.6.0] - 2026-03-27

### Added
- Claude Code plugin with 7 skills (setup, context, work, plan, handoff, triage, sync) and 3 session hooks (context injection, git sync, auto-handoff) (09d9f21)
- `story help <command>` extended help system with 18 agent-optimized topics (09d9f21)
- `story plugin install|uninstall claude-code` for one-command plugin management (09d9f21)
- `story init` now generates AGENTS.md by default for universal AI agent discoverability (09d9f21)

### Changed
- All 14 MCP tool descriptions expanded from 1-line to 2-4 sentences with usage guidance and cross-references (09d9f21)

_[manual]_
