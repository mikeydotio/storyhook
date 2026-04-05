# Documentation: Story Types & Epics

**Feature version**: v0.12.0
**Date**: 2026-04-05

## Table of Contents

1. [Feature Overview](#feature-overview)
2. [Architecture Decisions](#architecture-decisions)
3. [Data Model](#data-model)
4. [Configuration: types.toml](#configuration-typestoml)
5. [CLI Usage](#cli-usage)
6. [MCP Tool Integration](#mcp-tool-integration)
7. [Progress Rollup](#progress-rollup)
8. [Behavioral Notes](#behavioral-notes)
9. [Migration & Backward Compatibility](#migration--backward-compatibility)
10. [Export/Import](#exportimport)
11. [Doctor Checks](#doctor-checks)
12. [Implementation Map](#implementation-map)
13. [Test Coverage](#test-coverage)
14. [Known Gaps (ESCALATE)](#known-gaps-escalate)

---

## Feature Overview

Story Types & Epics adds configurable classification to storyhook stories. Types are stored as events in the event-sourcing model and configured via `types.toml`. Epics are a convenience layer built on top of typed stories and parent-of relationships -- they are not a separate entity.

The feature introduces:
- Five default story types: story, epic, bug, chore, task
- A `StoryTypeSet` event in the event-sourcing model
- CLI commands for type management (`story type`) and epic workflows (`story epic`)
- A `--type` filter flag on `story new`, `story set`, and `story list`
- Progress rollup computed on read for any story with children
- Parent-skip behavior in `story next` so parent stories are not surfaced as actionable work
- MCP tool parameters for type on create, update, and list operations
- Doctor integrity checks for type drift

## Architecture Decisions

### ADR-1: Epic = Typed Story (Not Separate Entity)

**Context**: Epics could be modeled as a distinct data structure with their own storage, or as regular stories with a type label and parent-of relationships.

**Decision**: Epics are stories with `story_type = "epic"` and `parent-of` relationships. The `story epic` subcommands are syntactic sugar that delegates to existing primitives (`story new --type epic`, `story relate ... parent-of ...`).

**Consequences**: No parallel code paths for epics vs stories. Any story can have children and show progress, not just epics. The `story epic` commands are thin wrappers. Downside: "epic" has no special semantics at the domain layer; it relies on convention.

### ADR-2: types.toml Mirrors states.toml

**Context**: The project already has a pattern for config-driven classification via `states.toml` with `StateDef` structs and load/save/add/remove operations.

**Decision**: types.toml follows the same structure and lifecycle as states.toml. `TypeDef` mirrors `StateDef`. Storage functions (`load_types`, `save_types`, `add_type`, `remove_type`) mirror their state counterparts.

**Consequences**: Consistent project configuration UX. Developers familiar with states.toml can immediately understand types.toml. No new patterns to learn.

### ADR-3: StoryTypeSet Mirrors StoryPrioritySet

**Context**: The event-sourcing model needs a new event for type changes.

**Decision**: `StoryEvent::StoryTypeSet { at: String, story_type: String }` follows the exact same pattern as `StoryEvent::StoryPrioritySet`. The fold function handles it identically: overwrite the previous value, update `updated_at`.

**Consequences**: Zero risk of introducing fold bugs since the pattern is proven. Additive change -- old event streams without `StoryTypeSet` still fold correctly (`story_type` defaults to `None`).

### ADR-4: Option<String> with serde(default) for Backward Compatibility

**Context**: Existing `StorySnapshot` structs serialized to archive.db and JSONL files do not contain `story_type`.

**Decision**: `StorySnapshot.story_type` is `Option<String>` with `#[serde(default)]`. Old stories deserialize with `story_type: None`. No migration of existing data is required.

**Consequences**: Zero-downtime upgrade. Existing projects continue working. Stories without a `StoryTypeSet` event display as "-" in human output and omit the field in JSON output (via `skip_serializing_if`).

### ADR-5: Progress Rollup Computed on Read, Never Stored

**Context**: Parent stories need to show how many children are done.

**Decision**: `compute_progress()` runs at view-build time in `build_story_views()`, counting direct `parent-of` children by superstate. The result is attached to `StoryView.progress` but never persisted as an event or in archive.db.

**Consequences**: Always consistent -- cannot go stale. No new events to manage. Slight cost on every list/show operation (iterates children), but with O(children) complexity per parent this is negligible for realistic project sizes.

### ADR-6: Direct Children Only for Progress

**Context**: Progress could count recursive descendants (grandchildren) or only direct children.

**Decision**: Direct children only. `compute_progress()` filters `parent-of` relationships and counts those IDs against the full story map.

**Consequences**: Simple, predictable behavior. A deeply nested epic hierarchy shows progress only for its immediate children. Users who need recursive visibility can check child epics individually. Avoids recursive graph traversal costs and complex display logic.

### ADR-7: Parent Skip in Next Handler Only

**Context**: Parents with children are technically "ready" (open, not blocked) but should not be surfaced as actionable work items.

**Decision**: The `Invocation::Next` handler filters out stories where `has_children()` returns true (i.e., stories with `parent-of` relationships). This filter is applied only in the Next handler, not in list or other views.

**Consequences**: `story next` surfaces leaf work items. `story list` still shows parents (with progress). The skip is a display/recommendation concern, not a domain rule.

### ADR-8: Epic Sugar Delegates to Primitives

**Context**: `story epic create/add/list/show` could duplicate logic or delegate.

**Decision**: Each epic subcommand maps directly to existing operations:
- `epic create` = `create_story()` + `StoryTypeSet { story_type: "epic" }`
- `epic add` = `StoryRelationshipAdded` with `parent-of` (reuses `validate_parent_constraints`)
- `epic list` = `build_story_views()` filtered by `story_type == "epic"`
- `epic show` = `story_view_by_id()` (same as `story show`)

**Consequences**: No code duplication. Epic commands benefit from all existing validation (cycle detection, self-reference checks, relationship constraints). If underlying primitives change, epic commands inherit the changes.

### ADR-9: Type Validation at Write Time Only

**Context**: The fold function (`fold_story`) processes events into snapshots. Should it validate types?

**Decision**: The fold is policy-free -- it applies events without checking whether types are valid. Type validation happens at write time in `app.rs` (commands that emit `StoryTypeSet` check against `load_type_map()`). The `doctor` command catches drift after the fact.

**Consequences**: Events are always replayable regardless of current types.toml configuration. If a type is removed from types.toml after stories have been tagged with it, those stories keep their type (doctor reports it as an issue). This separation keeps fold pure and testable.

### ADR-10: Lazy Migration via ensure_types_file

**Context**: Existing projects initialized before this feature do not have types.toml.

**Decision**: `load_types()` calls `ensure_types_file()`, which creates types.toml with default types if the file does not exist. `init_project()` also calls `ensure_types_file()` for new projects.

**Consequences**: Zero manual migration steps. First invocation of any type-related command (or any command that loads types) creates the file. Existing types.toml files are never overwritten.

### ADR-11: MCP Parameters on Existing Tools

**Context**: MCP integration could add new tools for types or extend existing ones.

**Decision**: `story_type` is added as an optional parameter to existing MCP tools: `storyhook_create_story`, `storyhook_update_story`, and `storyhook_list_stories`. No new MCP tools are introduced.

**Consequences**: MCP clients do not need to discover new tool names. Backward compatible -- omitting `story_type` works exactly as before. In `storyhook_update_story`, story_type follows the existing one-field-per-call priority chain: state > priority > labels > assignee > awaiting > story_type.

---

## Data Model

### TypeDef (domain.rs)

```rust
pub struct TypeDef {
    pub slug: String,
    pub description: Option<String>,
}
```

Stored in `types.toml`. The slug is the identifier used in events and commands.

### StoryTypeSet Event (domain.rs)

```rust
StoryEvent::StoryTypeSet {
    at: String,
    story_type: String,
}
```

Appended to a story's JSONL event stream. Overwrites any previous type during fold. Serialized as:

```json
{"kind":"StoryTypeSet","at":"2026-04-05T12:00:00Z","story_type":"epic"}
```

### StorySnapshot Extension (domain.rs)

```rust
pub struct StorySnapshot {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_type: Option<String>,
}
```

`None` when no `StoryTypeSet` event exists in the story's event stream.

### ProgressRollup (domain.rs)

```rust
pub struct ProgressRollup {
    pub children_done: usize,
    pub children_total: usize,
}
```

Computed by `compute_progress()`, attached to `StoryView.progress`. Never persisted.

---

## Configuration: types.toml

Located at `.storyhook/types.toml`. Created automatically on first access if missing.

### Default Contents

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

### Rules

- **First entry is the default type** -- returned by `default_type()` (currently used for reference, not auto-applied to untyped stories).
- **Slug "none" is reserved** -- `add_type` rejects it because `--type none` is used as a filter sentinel in `story list`.
- **At least one type must exist** -- `remove_type` rejects removal of the last type.
- **Types cannot be removed while in use** -- `remove_type` checks all open story snapshots.

---

## CLI Usage

### Type Management

```bash
# List all configured types with descriptions
story type list

# Add a custom type
story type add spike --description "A time-boxed investigation"

# Remove an unused type
story type remove spike
```

**Errors**:
- `story type add story` -- fails: "type `story` already exists"
- `story type add none` -- fails: "type slug `none` is reserved"
- `story type remove <in-use>` -- fails: "type `X` is still used by an existing story"
- `story type remove <last>` -- fails: "cannot remove the last type"

### Creating Typed Stories

```bash
# Create a story with an explicit type
story new "Fix login timeout" --type bug

# Create without type (story_type stays None)
story new "Set up CI pipeline"
```

### Changing Story Type

```bash
# Via story set
story set SH-5 --type chore

# Can combine with other fields
story set SH-5 --type bug --priority high --state in-progress
```

Type validation occurs at write time. The `--type` value must match a slug in types.toml.

### Filtering by Type

```bash
# Show only bugs
story list --type bug

# Show only untyped stories
story list --type none

# Combine with other filters
story list --type epic --state todo
```

### Epic Subcommands

```bash
# Create an epic (shorthand for story new + set type to epic)
story epic create "Auth System Overhaul"

# Add a child story to an epic
story epic add SH-1 SH-2    # SH-1 = epic (parent), SH-2 = child

# List all epics with progress
story epic list

# Show an epic with full detail and progress
story epic show SH-1
```

**`story epic create`** validates that the "epic" type exists in types.toml. If it has been removed, the error message tells the user how to re-add it.

**`story epic add`** creates a bidirectional `parent-of` / `child-of` relationship. It inherits all existing validation: no self-references, no cycles, single-parent constraint.

**`story epic list`** filters by `story_type == "epic"` and includes progress rollup in output.

**`story epic show`** is identical to `story show` -- it returns the full story view including progress and relationships.

### Output Format

**List view** (human-readable):
```
SH-1 [todo] [epic] Auth System Overhaul (2/3)
SH-2 [in-progress] [bug] Login timeout issue
SH-3 [done] [story] Add session expiry
```

The `[type]` badge appears after state. Progress appears as `(done/total)` for stories with children.

**Show view** (human-readable):
```
SH-1 Auth System Overhaul
state: todo (OPEN)
assignee: -
priority: none
type: epic
labels: -
flagged: no
relationships:
- parent-of SH-2
- parent-of SH-3
- parent-of SH-4
progress: 2/3 children done (66%)
```

**JSON output** (`--json`): `story_type` field is included when set, omitted when `None`. `progress` field is included when the story has children, omitted otherwise.

---

## MCP Tool Integration

### storyhook_create_story

Added optional parameter:
```json
"story_type": {"type": "string", "description": "Story type slug (e.g. story, epic, bug, chore, task)"}
```

Maps to `Invocation::New { title, state, story_type }`. Validated against types.toml before creation.

### storyhook_update_story

Added optional parameter:
```json
"story_type": {"type": "string", "description": "Set story type slug"}
```

Subject to the one-field-per-call limitation. Priority order: state > priority > labels > assignee > awaiting > story_type. If `story_type` is sent alongside higher-priority fields, only the highest-priority field is processed.

Maps to `Invocation::SetFields { ..., story_type: Some(value) }`.

### storyhook_list_stories

Added optional parameter:
```json
"story_type": {"type": "string", "description": "Filter by story type slug (e.g. bug, epic, story). Use 'none' for untyped stories"}
```

Maps to `Invocation::List { ..., story_type }`. Supports the `"none"` sentinel value to find untyped stories.

### Response Changes

`storyhook_get_story` and `storyhook_get_next` responses automatically include `story_type` and `progress` fields via serde serialization of `StoryView`. No schema changes needed -- the fields serialize when present and are omitted when absent.

---

## Progress Rollup

### How It Works

1. `build_story_views()` loads all open and archived story snapshots into a `BTreeMap<String, StorySnapshot>`.
2. For each story, `compute_progress(story, all_stories)` is called.
3. `compute_progress` collects all `parent-of` relationship targets (direct children only).
4. If no children exist, returns `None`.
5. Counts children where `superstate == SuperState::Closed` as "done".
6. Returns `ProgressRollup { children_done, children_total }`.
7. The rollup is stored in `StoryView.progress` for display and JSON serialization.

### Scope

- **Direct children only** -- grandchildren are not counted.
- **Includes archived children** -- a closed/archived child counts as "done".
- **Any parent story** -- not limited to epics. Any story with `parent-of` relationships gets progress.
- **Computed on every read** -- never cached or persisted.

### Display

- **List view**: `(2/3)` appended after the title
- **Show view**: `progress: 2/3 children done (66%)`
- **JSON**: `"progress": {"children_done": 2, "children_total": 3}`

---

## Behavioral Notes

### story next Skips Parents

The Next handler (`Invocation::Next`) filters candidates with:

```rust
.filter(|v| is_ready(&v.story, &story_map) && !has_children(&v.story))
```

A story is excluded from `story next` results if it has any `parent-of` relationship. This applies to all parents, not just epics. The rationale is that parent stories represent containers of work, not actionable items.

### Type Display for Untyped Stories

Stories without a `StoryTypeSet` event display `type: -` in show view and omit the type badge in list view. The `story_type` field is omitted (not null) in JSON output.

This is current behavior as shipped. An ESCALATE decision (SH-12) is pending on whether to display the default type name instead.

### Validation Boundaries

| Operation | Validates type against types.toml? |
|-----------|-----------------------------------|
| `story new --type X` | Yes |
| `story set --type X` | Yes |
| `story epic create` | Yes (checks "epic" exists) |
| `story import` with story_type | No (ESCALATE SH-13) |
| `story import-project` | No (saves types.toml from export) |
| `fold_story` | No (fold is policy-free) |
| `story doctor` | Yes (reports unknown types) |

---

## Migration & Backward Compatibility

### Existing Projects (Pre-v0.12.0)

No manual steps required. On the first command that calls `load_types()`, `ensure_types_file()` creates `.storyhook/types.toml` with the five default types. This happens transparently.

### Existing Stories

Stories created before this feature have no `StoryTypeSet` event. They fold with `story_type: None`. They display as untyped and can be filtered with `--type none`. They can be typed later with `story set <id> --type <slug>`.

### Event Stream Compatibility

`StoryTypeSet` is a new variant in the `StoryEvent` enum. Old event streams without this variant are unaffected. New event streams with this variant are forward-incompatible with pre-v0.12.0 binaries (they would fail to deserialize the unknown event kind).

### Archive Database

`StorySnapshot` has `#[serde(default)]` on `story_type`, so existing archived snapshots in archive.db (stored as JSON) deserialize correctly with `story_type: None`.

---

## Export/Import

### Export

`ProjectExport` includes a `types` field:

```rust
pub struct ProjectExport {
    pub schema: u32,
    pub prefix: Option<String>,
    pub states: Vec<StateDef>,
    #[serde(default)]
    pub types: Vec<TypeDef>,
    pub members: Vec<Member>,
    pub stories: Vec<ExportedStory>,
}
```

`export_project()` calls `load_types()` and includes all configured types. Story events include any `StoryTypeSet` events.

### Import

`import_project()` saves types from the export if the list is non-empty:

```rust
if !export.types.is_empty() {
    save_types(root, &export.types)?;
}
```

The `#[serde(default)]` annotation on the `types` field means exports from pre-v0.12.0 (which lack the field) import cleanly with an empty types vec, and the existing types.toml is preserved.

### Story-Level Import

`story import` writes `StoryTypeSet` events from `ImportStory.story_type` without validating against types.toml. This is intentional for migration flexibility but means `story doctor` should be run after bulk imports (ESCALATE SH-13).

---

## Doctor Checks

`story doctor` now includes a type integrity check. For each open story with a `story_type` value, it verifies the type exists in `load_type_map()`. If the type is not found:

```
SH-5: unknown type `spike`
```

This catches:
- Stories typed with a slug that was later removed from types.toml
- Stories imported with types not present in the current configuration
- Manual edits to JSONL files with invalid type values

`story doctor --fix` does not auto-fix type issues (no automated resolution is defined). The user must either re-add the type or re-type the story.

---

## Implementation Map

| File | What Changed | Lines of Interest |
|------|-------------|-------------------|
| `src/domain.rs` | `TypeDef` struct, `ProgressRollup` struct, `StoryTypeSet` event variant, `story_type` on `StorySnapshot`, fold handling, `has_children()`, `compute_progress()`, `last_activity_type` arm | TypeDef ~102-107, ProgressRollup ~110-113, StoryTypeSet ~205-208, fold ~332-338, has_children ~520-525, compute_progress ~527-556 |
| `src/storage.rs` | `TypesFile` struct, `types_file()` path, `default_types()`, `ensure_types_file()`, `load_types()`, `load_type_map()`, `save_types()`, `add_type()`, `remove_type()`, `default_type()`, `init_project` types call, `ProjectExport.types`, export/import types handling | TypesFile ~50-53, functions ~358-475, export ~847-854 |
| `src/cli.rs` | `TypeAction` enum, `EpicAction` enum, `Invocation::Type`, `Invocation::Epic`, `--type` on New/List/SetFields, `story_type` on List, help text | TypeAction ~29-33, EpicAction ~35-41, Invocation::Type ~243-245, Invocation::Epic ~246-248 |
| `src/app.rs` | Type validation on New/SetFields, `--type` filter on List, parent skip in Next, `EpicAction` handlers, type check in `doctor_report`, progress map in `build_story_views` | New validation ~45-67, List filter ~177-183, Next skip ~690, Epic handlers ~1985-2068, doctor ~2359-2374, progress ~2237-2242 |
| `src/mcp.rs` | `story_type` parameter on create/update/list schemas, `build_invocation` mapping | Schema ~152, ~178, ~196, ~299, invocation ~526, ~535, ~586-598 |
| `src/output.rs` | `progress` on StoryView, type badge in list render, type display in show render, progress display in show render | StoryView ~25, list ~256-265, show ~342-343, progress ~380-386 |
| `tests/story_types.rs` | 29 integration tests covering type CRUD, --type flag, epic subcommands, progress rendering, JSON output, MCP tools, E2E lifecycle | Full file |

---

## Test Coverage

**29 new integration tests** in `tests/story_types.rs`:

| Category | Tests | What They Cover |
|----------|-------|----------------|
| Type CRUD | 7 | list defaults, add, duplicate rejection, none rejection, no-description add, remove, remove-nonexistent |
| --type flag | 5 | new with type, set type, list filter, list --type none, type in show output |
| Epic subcommands | 5 | create, add child, list, show with progress, create without epic type |
| Progress rendering | 3 | progress in human output, progress in JSON output, no progress for leaf stories |
| MCP tools | 5 | create with type, update type, list filter by type, type in get_story response, type in get_next response |
| E2E lifecycle | 4 | full epic workflow, remove type rejected when in-use, remove last type rejected, doctor reports unknown type |

**Storage unit tests** added alongside the storage functions (lines ~1045-1260 in storage.rs):
- `ensure_types_file` creates defaults, does not overwrite existing
- `load_types` returns defaults, auto-creates if missing
- `load_type_map` returns BTreeMap keyed by slug
- `save_types` round-trips correctly
- `add_type` appends, rejects duplicates, rejects "none", handles no-description
- `remove_type` removes unused, rejects nonexistent, rejects in-use, rejects last
- `default_type` returns first slug, errors on empty

**Total test count**: 648 (619 baseline + 29 new integration tests), all passing.

---

## Known Gaps (ESCALATE)

Four stories are pending user decision. They represent design trade-offs, not bugs.

| Story | Title | Issue | Options |
|-------|-------|-------|---------|
| SH-12 | Default type display | Untyped stories show "-" instead of default type name from types.toml | Accept "-" (simpler) vs show default type (matches original design) |
| SH-13 | Import validation | `story import` does not validate story_type against types.toml | Add validation (consistent) vs keep permissive (flexible for migrations) |
| SH-14 | Progress bar format | Uses compact `(done/total)` instead of ASCII progress bar from design spec | Accept compact (clean CLI) vs add progress bar (visual) |
| SH-15 | Summary type breakdown | `story summary` / `story load-context` do not include type distribution | Accept as deferred scope vs implement now |

Triage recommendations (from `.forge/TRIAGE.md`): SH-12 Option 1, SH-13 Option 2, SH-14 Option 1, SH-15 Option 1.
