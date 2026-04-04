# Architecture Design: Story Types & Epics

## System Overview

Types integrate as classification metadata through storyhook's existing event-sourcing model. The flow is: `types.toml` config → `StoryTypeSet` events → fold into `StorySnapshot` → display with progress rollup. No new dependencies, no schema migrations, no breaking changes.

```
┌────────────────────────────────────────────────┐
│           CLI / MCP Entry Points               │
│  story new --type epic "Auth"                  │
│  story epic list                               │
│  storyhook_create_story { type: "epic" }       │
└─────────────────────┬──────────────────────────┘
                      │
      ┌───────────────▼─────────────────────┐
      │  app.rs: validates type against     │
      │  types.toml, emits StoryTypeSet,    │
      │  computes progress rollup on read   │
      └───────┬─────────────┬───────────────┘
              │             │
    ┌─────────▼──────┐ ┌───▼──────────────┐
    │  storage.rs    │ │  domain.rs       │
    │  types.toml    │ │  StoryTypeSet    │
    │  load/save     │ │  fold + rollup   │
    └─────┬──────────┘ └────┬─────────────┘
          │                 │
    ┌─────▼─────────────────▼──────────────┐
    │  .storyhook/                         │
    │  types.toml       (config)           │
    │  stories/*.jsonl  (events)           │
    │  archive.db       (snapshots)        │
    └──────────────────────────────────────┘
```

## Components

### domain.rs — Event & Snapshot Changes

- **Purpose:** Define `StoryTypeSet` event, extend `StorySnapshot` with `story_type`, fold the new event, provide `has_children` and `compute_progress` helpers.
- **Interfaces:**
  - `StoryEvent::StoryTypeSet { at: String, story_type: String }` — new event variant mirroring `StoryPrioritySet`
  - `TypeDef { slug: String, description: Option<String> }` — config struct mirroring `StateDef`
  - `StorySnapshot.story_type: Option<String>` — `None` maps to default type from `types.toml` at display time
  - `ProgressRollup { children_done: usize, children_total: usize }` — computed on read, never stored
  - `has_children(story: &StorySnapshot) -> bool` — checks for `parent-of` relationships
  - `compute_progress(story, all_stories) -> Option<ProgressRollup>` — direct children only, done = superstate CLOSED
- **Dependencies:** None (innermost layer)
- **Key decisions:**
  - `StoryTypeSet` follows `StoryPrioritySet` pattern exactly (domain.rs:184-187)
  - `Option<String>` with `#[serde(default)]` for backward compat — old snapshots deserialize as `None`
  - `None` maps to default type (first entry in `types.toml`, e.g., "story") at display time — not stored as an event
  - Progress rollup: direct children only, `superstate == Closed` = done, includes archived children
  - New arm in `last_activity_type`: `StoryTypeSet { .. } => "type-set"`

### storage.rs — Config Loading & Persistence

- **Purpose:** Manage `types.toml` lifecycle: path resolution, loading, saving, CRUD, default creation, auto-migration.
- **Interfaces:**
  - `ProjectPaths::types_file() -> PathBuf` — `.storyhook/types.toml`
  - `load_types(root) -> Result<Vec<TypeDef>>` — loads types, auto-creates file if missing
  - `load_type_map(root) -> Result<BTreeMap<String, TypeDef>>` — keyed by slug
  - `save_types(root, types) -> Result<()>`
  - `add_type(root, slug, description) -> Result<TypeDef>` — validates no duplicates, rejects reserved slugs ("none")
  - `remove_type(root, slug) -> Result<()>` — rejects if any open story uses the type
  - `ensure_types_file(root) -> Result<()>` — creates with defaults if missing
  - `default_type(root) -> Result<String>` — returns slug of first type in `types.toml`
- **Dependencies:** `domain::TypeDef`, `error::AppError`
- **Key decisions:**
  - `ensure_types_file` called lazily from `load_types`, not on every command
  - `init_project` also calls `ensure_types_file` so new projects start with it
  - Default types: story (default), epic, bug, chore, task
  - Reserved slugs: "none" is forbidden in `add_type`
  - `remove_type` checks for in-use types before allowing deletion

#### types.toml Format

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

First entry is the default type. Stories without a `StoryTypeSet` event display as this type.

### cli.rs — Command Parsing

- **Purpose:** Parse new commands and flags into `Invocation` variants.
- **Interfaces:**
  - `TypeAction { List, Add { slug, description }, Remove { slug } }` — new enum
  - `EpicAction { List, Show { id }, Create { title }, Add { epic_id, story_id } }` — new enum
  - `Invocation::Type { action: TypeAction }` — new variant
  - `Invocation::Epic { action: EpicAction }` — new variant
  - `Invocation::New` gains `story_type: Option<String>`
  - `Invocation::List` gains `story_type: Option<String>`
  - `Invocation::SetFields` gains `story_type: Option<String>`
- **Dependencies:** `error::AppError`
- **Key decisions:**
  - `parse_type` and `parse_epic` follow the `parse_phase` pattern
  - `--type` flag added to `parse_new`, `parse_list`, `parse_set`
  - Dispatch in `parse_invocation`: `"type" => parse_type`, `"epic" => parse_epic`

#### CLI Commands

```
# Type management
story type list
story type add <slug> [--description "<text>"]
story type remove <slug>

# Epic sugar (delegates to existing primitives)
story epic list                    # list --type epic + progress
story epic show <id>               # story show with progress
story epic create "<title>"        # story new --type epic
story epic add <epic-id> <story-id> # story relate parent-of

# Modified existing commands
story new "<title>" [--state <slug>] [--type <slug>]
story set <id> [--type <slug>]
story list [--type <slug>]
```

### app.rs — Command Handlers

- **Purpose:** Business logic for type/epic commands, progress rollup, list filtering, next-story parent skipping.
- **Interfaces:** Handlers for `Invocation::Type`, `Invocation::Epic`, modified `New`/`List`/`SetFields`/`Next`/`Doctor`
- **Dependencies:** `storage`, `domain`, `cli::Invocation`, output types
- **Key decisions:**
  - Type handlers follow `StateAdd`/`StateRemove` pattern (app.rs:72-88)
  - Epic handlers delegate to existing primitives (create → new + type set, add → relate, list → list filter, show → story show)
  - `epic create` emits both `StoryCreated` and `StoryTypeSet` inside same `with_project_lock` closure
  - Type validation at write time: check slug exists in `load_type_map`
  - `--type` filter in list handler: after existing phase filter (~line 154)
  - `--type none` filters to `story_type.is_none()` (untyped stories)
  - Parent skip in `Next` handler only (not in `is_ready`) — parents are "ready" but not "actionable"
  - Progress rollup computed in `build_story_views` for every story with children
  - Doctor gains type integrity check: warn on unknown types

### mcp.rs — MCP Tool Updates

- **Purpose:** Expose type field in MCP tool schemas.
- **Interfaces:**
  - `storyhook_create_story` gains `story_type: Option<String>` param
  - `storyhook_update_story` gains `story_type: Option<String>` param
  - `storyhook_list_stories` gains `story_type: Option<String>` param
  - `storyhook_bulk_create` gains `story_type` per item
  - `storyhook_get_story` and `storyhook_get_next`: no schema change (type + progress auto-included via serde)
- **Dependencies:** `cli::Invocation`, `app::run`
- **Key decisions:** No new MCP tools. Story type is a parameter on existing tools, not a separate tool.

### output.rs — Display Changes

- **Purpose:** Render type and progress in human-readable and JSON output.
- **Interfaces:**
  - `StoryView` gains `progress: Option<ProgressRollup>` field
  - `render_story` displays type after priority line, progress after relationships
  - List rendering shows `[type]` badge and progress bar for parents
- **Dependencies:** `domain` types
- **Key decisions:**
  - When displaying, `story_type: None` renders as the default type from `types.toml`
  - JSON output includes raw `story_type` (may be `null` for old stories) and `progress` via serde

#### Output Examples

```
# story show SH-1
SH-1 Auth System
state: todo (OPEN)
priority: high
type: epic
progress: 4/5 children done (80%)

# story epic list
SH-1 [epic] Auth System     ████████░░ 80% (4/5)  todo
SH-5 [epic] Data Pipeline   ██░░░░░░░░ 20% (1/5)  in-progress

# story list --type bug
SH-3 [bug] Login crash                            in-progress
SH-7 [bug] Timeout on save                        todo
```

## Data Flow

### Setting a Type

```
story new --type epic "Auth System"
  1. cli.rs: parse_new → Invocation::New { title, story_type: Some("epic") }
  2. app.rs: validates "epic" exists in types.toml via load_type_map()
  3. storage.rs: create_story() writes StoryCreated to SH-1.jsonl
  4. app.rs: writes StoryTypeSet { story_type: "epic" } to SH-1.jsonl
  5. storage.rs: fold events → StorySnapshot { story_type: Some("epic"), ... }
  6. output.rs: render with type badge
```

### Progress Rollup

```
story epic show SH-1
  1. app.rs: build_story_views() loads all open + archived snapshots
  2. For SH-1, calls compute_progress(SH-1, all_stories)
  3. domain.rs: finds parent-of relationships → [SH-2, SH-3, SH-4]
  4. Checks each child's superstate: SH-2=Closed, SH-3=Open, SH-4=Closed
  5. Returns ProgressRollup { children_done: 2, children_total: 3 }
  6. output.rs: shows "progress: 2/3 children done (66%)"
```

### Default Type Resolution

```
story show SH-99  (old story, no StoryTypeSet event)
  1. fold_story: story_type = None (no event to set it)
  2. output.rs: load default_type() from types.toml → "story"
  3. Display: "type: story"
  4. JSON: story_type: null (raw value preserved for API consumers)
```

### Story Next (Parent Skipping)

```
story next
  1. build_story_views() → all views
  2. Filter to is_ready() stories (no blockers)
  3. Filter out has_children() stories  ← NEW (in Next handler only)
  4. Sort by priority, then created_at
  5. Return first result
```

## Cross-Cutting Concerns

### Backward Compatibility

| Layer | Strategy |
|-------|----------|
| Event logs | `StoryTypeSet` is additive. Old logs without it produce `story_type: None` via fold. |
| StorySnapshot JSON | `#[serde(default)]` on `story_type`. Old JSON deserializes cleanly. |
| Archive DB | No SQLite schema change. Type lives inside `snapshot_json` blob. |
| CLI | All new flags are optional. Existing syntax unchanged. |
| MCP | All new params are optional. Existing clients work without modification. |
| Config | `types.toml` auto-created on first type-related use. |

### Error Handling

| Scenario | Error | Message Pattern |
|----------|-------|-----------------|
| Unknown type on write | `AppError::Validation` | `type \`{slug}\` is not defined. Available types: ...` |
| Remove in-use type | `AppError::Validation` | `type \`{slug}\` is still used by an existing story` |
| Duplicate type add | `AppError::Validation` | `type \`{slug}\` already exists` |
| Reserved slug "none" | `AppError::Validation` | `type slug \`none\` is reserved` |
| Epic add cycle | Existing cycle detection | Reused from relate handler |
| types.toml parse error | `AppError::Storage` | Via `toml::de::Error` |
| Doctor: unknown type | `AppError::Integrity` | `{id}: unknown type \`{slug}\`` |

### Migration Strategy

- **New projects:** `init_project` calls `ensure_types_file` — `types.toml` created alongside `states.toml`
- **Existing projects:** `types.toml` auto-created with defaults on first `load_types()` call (triggered by any type-related command or `story doctor`)
- **No user action required.** Migration is transparent and non-destructive.

## Integration Points

### Epic Sugar → Existing Primitives

| Epic Command | Delegates To |
|-------------|-------------|
| `story epic create "Title"` | `story new "Title"` + `StoryTypeSet { type: "epic" }` |
| `story epic add <eid> <sid>` | `story relate <eid> parent-of <sid>` |
| `story epic list` | `story list --type epic` + progress rollup |
| `story epic show <id>` | `story show <id>` (progress already attached in `build_story_views`) |

### Progress Rollup → build_story_views

Progress is computed in `build_story_views` (app.rs:2042) for every story that has `parent-of` relationships. The full story map (open + archived) is already loaded at this point. `ProgressRollup` is attached to `StoryView` for rendering.

### Doctor → Type Integrity

Doctor loads `types.toml` via `load_type_map` (triggers auto-migration) and checks each story's `story_type` against the map. Unknown types are flagged as integrity issues.

## Design Decisions

| Decision | Choice | Rationale | Alternatives Considered |
|----------|--------|-----------|------------------------|
| Event variant | `StoryTypeSet` | Mirrors `StoryPrioritySet`. Additive, non-breaking. | Modify `StoryCreated` (breaks compat) |
| Type field type | `Option<String>` | `None` = default type. String because user-configurable. | `String` with hardcoded default (couples domain to config) |
| Default type | First entry in `types.toml` | Every story has a displayable type. "Clearing" = set to default. | `None` = untyped (irreversible state) |
| Reserved slugs | "none" forbidden | Prevents collision with `--type none` filter. | `--untyped` flag (flag proliferation) |
| Progress scope | Direct children only | User requirement. Recursive is confusing at depth > 2. | Recursive (complex), weighted (over-engineered) |
| Archived children | Count toward progress | CLOSED is CLOSED regardless of storage. | Exclude archived (undercounts after auto-archive) |
| `story next` parent skip | Always skip, Next handler only | Parents are containers, not actionable. `is_ready` unchanged. | In `is_ready()` (changes list/summary semantics), `--no-parents` flag (YAGNI) |
| Type validation | Write-time only | Fold reconstructs state without policy. Doctor catches drift. | During fold (couples replay to config) |
| Migration | Lazy on first `load_types()` | Avoids noise for users who don't use types. | Eager on every command (noisy) |
| Epic subcommand | Sugar over primitives | No parallel code paths. Delegates to existing tested machinery. | Separate epic entity (against design philosophy) |
| TypeDef fields | slug + description | Minimal. Matches StateDef. Color/icon are presentation. | Add color/icon/role (premature) |
| GitHub sync | Deferred | Issue Types API is immature. | Sync now (fragile) |
| Auto-typing in decompose | No | Type is classification, not behavioral. | Auto-type parent as epic (assumes intent) |
| TUI changes | Deferred | Display-only, no architecture impact. Separate task. | Include now (scope creep) |
| MCP approach | Add params to existing tools | No new tools needed. Follows existing pattern. | New storyhook_epic_* tools (duplication) |

## Files Changed Summary

| File | Change Type | Scope |
|------|-------------|-------|
| `src/domain.rs` | Modify | `StoryTypeSet` event, `TypeDef` struct, `story_type` on snapshot, fold logic, `has_children`, `compute_progress`, `last_activity_type` arm |
| `src/storage.rs` | Modify | `types_file()` path, `TypesFile` struct, `load_types`, `save_types`, `add_type`, `remove_type`, `ensure_types_file`, `default_type`, `init_project` update |
| `src/cli.rs` | Modify | `TypeAction`/`EpicAction` enums, new `Invocation` variants, `story_type` on `New`/`List`/`SetFields`, new parsers, help text |
| `src/app.rs` | Modify | `Type`/`Epic` handlers, type validation in `New`/`SetFields`, `--type` filter in `List`, parent skip in `Next`, progress in `build_story_views`, doctor type check |
| `src/mcp.rs` | Modify | `story_type` param in create/update/list schemas, `build_invocation` updates |
| `src/output.rs` | Modify | `progress` on `StoryView`, type + progress display in rendering |

## Open Questions for Planning

1. Should `story summary` and `story context` include type breakdown? (Likely yes, but implementation details for planning.)
2. Should `story export` include `types.toml` data in the export format?
3. Should `story decompose` accept type annotations in specs (e.g., `[EPIC]` markers)?
4. TUI type display — deferred but should be tracked as future work.
