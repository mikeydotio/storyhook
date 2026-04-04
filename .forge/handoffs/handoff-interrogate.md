# Handoff: Interrogate → Research

## Step Completed
interrogate

## Key Decisions
- **Epics are story types**, not separate entities. User explicitly stated: "Epic should be a story type like Bug or Chore."
- **Configurable type system** via `types.toml`, mirroring the `states.toml` pattern. Ship with defaults: story, epic, bug, chore, task.
- **Progress rollup is universal** — any story with children shows completion %, not just epics. User clarified: "any given story's completion should be as a percentage of its immediate children that are complete, unless it has no children."
- **Direct children only** for progress computation (not recursive).
- **`story next` skips parents** — stories with children are excluded from next recommendations by default.
- **Types are purely classification** — no behavioral roles. Behavior is driven by parenthood (has children → progress rollup, skip in next), not by type.

## Context for Next Step
### Top Requirements
1. Configurable `types.toml` with defaults (story, epic, bug, chore, task)
2. `StoryTypeSet` event in event-sourcing model
3. CLI: `story type add/remove/list`, `story new --type`, `story set --type`
4. CLI: `story epic list/show/create/add` as ergonomic sugar
5. Universal progress rollup for any parent story (% of direct children that are CLOSED)
6. `story next` filters out stories with children
7. MCP tools updated with type field

### Architecture Notes
- Storyhook is Rust, event-sourced (JSONL), with SQLite archive for closed stories
- States are configurable via `states.toml` — types should follow the same pattern
- Phases use label convention (`phase:N`) — types use a dedicated field, not labels
- Parent-child relationships already exist with single-parent enforcement and cycle detection
- The codebase has 31 integration test files; new tests needed for types and epic commands

### Existing Patterns to Follow
- `states.toml` for type configuration file format
- `StoryPrioritySet` event for the `StoryTypeSet` event pattern
- `story phase list/show/create/add/remove` for `story epic` and `story type` subcommand patterns
- `story list --label/--priority/--state` for `story list --type` filter

### User Preferences
- User values non-invasive tool design and plans ahead of dogfooding
- This is a public-facing feature — design must be general, not just for personal use
- User wants storyhook to be a CLI-first, Claude Code plugin that's easy to adopt and easy to ditch

## Open Questions for Research
- Best practices for type systems in event-sourced models (schema evolution)
- How to handle type in archived stories (SQLite) — does the archive schema need updating?
- GitHub sync implications — should types map to GitHub issue types?
- TUI display considerations for types and progress bars
- Whether `story decompose` should auto-type parent stories as epics
- How other Rust CLI tools implement configurable entity types
