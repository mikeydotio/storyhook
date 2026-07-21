# Changelog

All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [v0.16.0] - 2026-07-21

### Added
- add --if-state CAS guard to story move (30ab541)

### Fixed
- account for soft-deletion in --if-state CAS guard (728a7bd)
- give --if-state an unambiguous parse boundary in story move (f5fdc9b)
- close CAS review gaps in --if-state move guard (b3dfa12)
- run cargo-version sync as a pre-bump hook (1896992)

### Changed
- Merge pull request #33 from mikeydotio/feat/move-if-state (3a71fac)
- Merge pull request #32 from mikeydotio/fix/semver-sync-cargo-pre-bump (838c611)
- Merge pull request #31 from mikeydotio/chore/release-v0.15.0 (1f7d880)

_[manual]_

## [v0.15.0] - 2026-07-20

### Added
- add `story web open` and `story web address` (b1a73db)
- animate live dashboard updates by type of change (0caceaa)
- push live story updates to the dashboard over SSE (71e6659)
- add home/repo/settings screens to the dashboard frontend (cb185d8)
- make the dashboard registry-backed, one global daemon (808efe9)
- add ~/.storyhook/registry.toml repo registry (174d8d9)
- board + list dashboard with drawer and drag-and-drop (a828e0f)
- add mutation API with CSRF/DNS-rebinding guard (bb4351b)
- add GET /api/story/{id} and ordered /api/data meta (06fbafc)

### Fixed
- replace stray NUL byte in dashboard fingerprint separator (dcb71f2)
- supply missing force field in reopen route (735892a)
- supply Invocation::Reopen.force in the web reopen route (2a81190)
- bind loopback + tailnet only, never the public internet (a0c8ace)
- deleting a story now closes it, not just archives it (#18) (38f41ca)
- inject session-start context via additionalContext (silent) (70e3dd1)
- make sync-cargo-toml hook portable on BSD/macOS sed (faa0866)

### Changed
- Merge pull request #30 from mikeydotio/feat/web-open-address (1a7ca00)
- Merge pull request #29 from mikeydotio/worktree-sto-21 (431905b)
- Merge pull request #28 from mikeydotio/chore/untrack-stray-worktree-gitlink (142ac9f)
- Merge pull request #27 from mikeydotio/feat/multi-repo-web-dashboard (01635a8)
- Merge pull request #24 from mikeydotio/fix/23-web-reopen-force (8763004)
- Merge pull request #19 from mikeydotio/worktree-sto-17 (fd09e61)
- Merge pull request #22 from mikeydotio/worktree-sto-18 (8f58321)
- dedupe delete_story onto archive_story (f1be5bb)
- centralize security headers, add router scaffold (365d168)

### Documentation
- document the multi-repo dashboard (e9f2243)
- correct Makefile's false claim of CI parity (7d02de4)
- document the interactive dashboard and write API (4d5553b)

### Testing
- retarget grafted #23 reopen regressions at registry-backed API (d161006)

### Maintenance
- untrack stray .claude/worktrees gitlink and ignore the dir (6f911ca)
- bump plugin to 0.2.1 for silent session-start context (8222173)
- bump plugin to 0.2.0 to ship storyhook-update skill (dadd8d5)
- remove test workflow; tests run locally via make test (e362c56)

_[manual]_

## [v0.14.0] - 2026-07-03

### Added
- add storyhook-update skill (cba8213)
- add `story update` self-update command and `--version` flag (0b57ccf)
- add storyhook-install skill and CLI-presence guards (ae8d2de)
- register via Claude Code marketplace instead of copying (5274a65)
- implement web dashboard (story web start/stop/status) (5b6afe9)

### Fixed
- avoid a python3 spawn on every non-git Bash call (F074) (7cba1d6)
- no-op Stop-hook handoff when a forge pipeline is active (F072) (995b6e1)
- allowlist command -v in storyhook-setup skill (F073) (a11a7a9)
- storyhook-triage emits real verb-first mutations (d2287bf)
- storyhook-work emits real verb-first mutations (2de7edb)
- correct hooks.json to valid Claude Code schema (55f232c)
- address all 5 triage FIX items for web dashboard (2542fb8)

### Changed
- Merge pull request #3 from mikeydotio/worktree-web-ui (0c806ff)
- integrate origin/main into web-ui branch (ec801fe)
- project documentation for web dashboard (24f7488)
- 5 FIX, 0 ESCALATE — all findings have clear solutions (37b5dc7)
- static analysis and test hardening complete (63633e6)
- web dashboard implementation plan approved (9e147c7)

### Documentation
- fix workflow-patterns.md to verb-first command grammar (1636303)
- rewrite cli-reference.md to the real verb-first CLI (5aa6f6d)
- document Claude Code plugin install routes (5beae1c)

### Maintenance
- add Makefile mirroring CI checks (7cdf175)
- satisfy fmt + clippy on current stable toolchain (2801271)

_[manual]_

## [v0.13.0] - 2026-04-07

### Added
- rewrite session-start hook to use story session-start (5634aa9)
- remove MCP references from scaffold templates (6900939)
- add story session-start CLI command (9f98263)
- add --compact and --all flags to help system (871bf2d)
- remove MCP from documentation and plugin files (de6f31c)
- strip MCP server from Rust codebase (c32b117)
- make reserved slug "none" check case-insensitive (55bf68d)
- add story_type to JSON patch dispatch table (f574b7d)
- fix all clippy warnings for clean validation (7686399)
- add type breakdown test for HTML report (1527253)
- add type breakdown to Context handler (ccb2399)
- add type breakdown to summary and report output (35852aa)
- add import validation for story_type against types.toml (aed79cf)
- display "Default" for untyped stories + reserve "default" slug (84558db)
- add story_type to MCP update tool description (8f41b4b)
- add types to export/import and StoryTypeSet on import (5f022b8)
- add story_type param to MCP tool schemas and update handler (6d6988b)
- add progress rollup, parent skip in Next, doctor type check (2810707)
- add Type and Epic command handlers to app.rs (d2b93e4)
- add type + progress rendering to output.rs (d0d65c3)
- add types.toml config lifecycle to storage.rs (87dec1a)
- add TypeAction, EpicAction, Invocation variants, parsers, --type flag (b80ad6b)
- add StoryTypeSet event, TypeDef, ProgressRollup, story_type field (e33c2f0)

### Fixed
- Add CLI alternative to plugin install success message (d6e116d)
- Add CHANGELOG entry for MCP removal breaking change (d67848c)
- Sync Cargo.toml version to 0.12.0 and add post-bump hook (8bd17a6)
- Remove ghost --tree reference from scaffold template (5a40484)
- Add --compact and --all flags to help usage line (a555475)
- RawJson output bypasses --quiet flag (1da9cac)
- proper TOML parsing for plugin-config check (0b49f33)
- UTF-8 safe truncation in session-start (5eeb49f)
- guard against removing last type in remove_type (c665f76)
- sync storyhook state for SH-7 and SH-8 (missed during execute sessions) (0642d07)

### Changed
- pipeline complete — pushed to origin/main (3564d85)
- Fix cycle 5 complete — all 5 ESCALATE stories done (146db71)
- Decomposition complete — 5 ESCALATE stories mapped, plan-mapping.json created, DAG valid (2bbdffb)
- Fix cycle 5 plan approved — 5 ESCALATE stories, 1 wave, ready for decomposition (182adc7)
- ESCALATE review complete — 5 stories approved with recommended approaches, dispatching to plan (6c67314)
- SH-33 complete — 3/8 fix cycle stories done, pausing (b160ca6)
- SH-32 complete — 2/8 fix cycle stories done, pausing (75edc69)
- SH-31 complete — 1/8 fix cycle stories done, pausing (1abd52f)
- Decomposition complete — 8 stories mapped to plan tasks, parent SH-40, DAG valid (78d0e7e)
- FIX cycle 4 plan approved — 8 tasks in 1 wave (81ce55e)
- Project documentation complete — ready for ESCALATE review (1d21db0)
- 0 FIX, 9 ESCALATE (max fix cycles reached) (d8ab754)
- static analysis and validation reports (9b41d66)
- test hardening + 24 new tests (6b414a1)
- all stories complete (603f17d)
- SH-36 complete — session paused (1/1 stories) (1ece1f4)
- SH-35 complete — session paused (1/1 stories) (51fa5a0)
- SH-34 complete — session paused (1/1 stories) (887f4ed)
- SH-33 complete — session paused (1/1 stories) (5ed11ee)
- SH-32 complete — session paused (1/1 stories) (5fb06ba)
- Decomposition complete — 6 stories created (SH-32 to SH-37) across 4 waves (8ca3fb8)
- implementation plan approved — replace MCP with CLI documentation (50ab759)
- all fix cycle 3 stories complete — transitioning to review+validate (77d9661)
- SH-29 done — story_type JSON patch dispatch, session paused (5e51c1f)
- create stories from fix cycle 3 plan (7a5073f)
- fix cycle 3 — 2 FIX items planned, 6 red tests pre-written (0ca5fef)
- 2 FIX, 0 ESCALATE — cycle 2 (2b1b93e)
- test hardening + report -- cycle 2 (af620bb)
- add missing tests and fix unused import warning (92e4790)
- static analysis complete — cycle 2 (2d283e9)
- all stories complete — transitioning to review+validate (5af5c75)
- SH-26 complete — session 5 paused (1/1 stories) (18f799e)
- SH-25 complete — session 4 paused (1/1 stories) (f93a8e6)
- SH-24 complete — session 3 paused (1/1 stories) (7cc23b5)
- SH-23 complete — session 2 paused (1/1 stories) (a0f3eda)
- SH-22 complete — session 1 paused (1/1 stories) (2444dd5)
- create 6 task stories from ESCALATE fix cycle plan (b1ddddb)
- ESCALATE fix cycle plan approved — 6 tasks, 3 waves (d573f23)
- archive cycle-1, record user decisions on 4 ESCALATE stories (e590a24)
- project documentation for Story Types & Epics (301dfa0)
- fix cycle 1 complete — all 3 stories done (0 retries) (3469293)
- remove dead code branch in progress rendering (10ed49a)
- fix cycle 1 — SH-17 complete, session paused (1/3) (3f8e9de)
- fix cycle 1 — 3 stories from triage FIX items (7fd1ba4)
- fix cycle 1 — 3 tasks, 1 wave approved (62be55c)
- 3 FIX, 4 ESCALATE (626d12f)
- add handoff document (b713fc8)
- test hardening + report (9ad2d6b)
- static analysis complete (3933ab4)
- all stories complete (9/9 sessions, 11/11 stories) (630a9b8)
- SH-10 complete, session paused (8/9 stories) (8e49cf5)
- SH-9 complete, session paused (7/9 stories) (e8e449b)
- SH-8 complete, session paused (6/9 stories) (8f660c7)
- SH-7 complete, session paused (5/9 stories) (505af5e)
- SH-6 complete, session paused (4/9 stories) (da76aff)
- SH-5 complete, session paused (3/9 stories) (4761c2d)
- SH-4 complete, session paused (2/9 stories) (c6ba348)
- SH-3 complete, session paused (1/1 stories) (90e820d)
- create stories from plan (5d93266)
- implementation plan approved (a0ad016)
- architecture design approved (8f22f44)
- domain research + team roster (084a94d)
- capture idea — Story Types & Epics (8808f62)

### Testing
- Update test to verify post-bump hook instead of config.yaml (de73026)

### Maintenance
- track tool config (.storyhook, .semver, .planning) (1866a1b)
- sync storyhook state for SH-10 completion (cf9286d)
- track tool config (.storyhook, .semver, .planning) (462e90a)

_[manual]_

## [v0.12.0] - 2026-03-31

### Added
- **Phase support** — organize stories into phases using `phase:N` labels (convention on existing labels, zero storage changes)
- `story phase list` — per-phase progress overview with completion counts
- `story phase show <N>` — list stories in a specific phase
- `story phase add <id> <N>` — assign story to phase (auto-strips old phase assignment)
- `story phase remove <id>` — clear phase assignment
- `story phase create <N> ["<title>"]` — create a named grouping story for a phase
- `--phase <N>` filter on `story list` and `story next` — scope queries to a specific phase
- `story load-context` — renamed from `story context` for clarity; auto-detects phases and includes Phase Progress section
- `### Wave N` in `story decompose` now preserves phase identity via `phase:N` labels (previously lost after import)
- `storyhook_phase_list` MCP tool and `phase` parameter on `storyhook_list_stories` / `storyhook_get_next`
- Phase number validation — must be a positive integer
- 11 new integration tests for phase commands

### Changed
- `story context` renamed to `story load-context` (old name kept as alias)
- Phase counting uses state roles instead of hardcoded "in-progress" — works with custom active states

### Removed
- **MCP server removed** — the built-in JSON-RPC server is no longer part of storyhook
- `story --mcp` flag — no longer available; the MCP server process cannot be launched
- `story mcp-config` command — MCP configuration is no longer needed
- **Migration path**: session hooks via `story session-start` replace MCP for AI agent integration. Run `story plugin install claude-code` to set up hooks automatically.

_[manual]_

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
