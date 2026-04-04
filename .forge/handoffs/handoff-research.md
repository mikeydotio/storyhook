# Handoff: Research → Design

## Step Completed
research

## Key Decisions
- **Additive event evolution** — `StoryTypeSet` as new event variant, not modifying `StoryCreated`. Follows `StoryPrioritySet` pattern exactly.
- **`Option<String>` for type** — `None` means untyped, not implicit default. Backward compatible via `#[serde(default)]`.
- **No archive migration needed** — Type lives inside `snapshot_json` blob. SQLite schema unchanged.
- **Types validated at write time only** — `fold_story` doesn't validate types, matching state handling pattern.
- **Defer GitHub type sync** — Issue Types API is immature. Store type in storyhook only for now.
- **No auto-typing** — `story decompose` doesn't auto-set parent as epic. Type is classification, not behavioral.
- **Minimal TypeDef** — slug + optional description only. No color, icon, or role fields.
- **No conditional agents needed** — CLI tool with pattern-following implementation. Core team sufficient.

## Context for Next Step

### Research Summary
The implementation follows five established codebase patterns:
1. `states.toml` → `types.toml` (config file format)
2. `StoryPrioritySet` ��� `StoryTypeSet` (event model)
3. `story phase` → `story type` + `story epic` (subcommand sugar)
4. `--state`/`--priority` → `--type` (list filter)
5. `PRAGMA table_info` migration → future archive column

### Codebase Locations
- Event enum: `src/domain.rs:149`
- StorySnapshot: `src/domain.rs:124`
- fold_story: `src/domain.rs:244`
- States loading: `src/storage.rs:277`
- ProjectPaths: `src/storage.rs:55`
- Archive schema: `src/storage.rs:861`
- CLI parsing: `src/cli.rs`
- MCP tools: `src/mcp.rs`
- Doctor checks: `src/app.rs:2187`
- Parent-child: `src/domain.rs:401` (inverse_relation)
- Next story: `src/app.rs:2477`

### Team Roster
Core team only. No UX designer (CLI patterns clear), no security researcher (no attack surface), no accessibility engineer (text-only output).

### Patterns to Follow
- `types.toml` mirrors `states.toml` with `[[types]]` sections
- `TypeDef { slug: String, description: Option<String> }`
- Progress rollup: computed on read, never stored. `children_done / children_total` where done = superstate CLOSED
- `story next` filters: exclude stories with `parent-of` relationships

## Open Questions for Design
1. Should `story epic remove` exist as unrelate sugar?
2. Should `story next` skip-parents behavior be implicit or behind `--no-parents` flag?
3. TUI display for types and progress bars
4. Should closed children count toward parent progress? (Recommendation: yes)
5. Should `story decompose` accept `--parent-type` flag?
