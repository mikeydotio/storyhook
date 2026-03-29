# Changelog

All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/).

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
