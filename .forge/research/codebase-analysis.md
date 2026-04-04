# Research: Storyhook Codebase Analysis

## Architecture Overview

Storyhook is a Rust CLI tool with event-sourcing (JSONL per story) and SQLite archive. Key source files:

| File | Purpose | Lines (approx) |
|------|---------|------|
| `src/domain.rs` | Core domain types, events, fold_story, relationships | ~1400 |
| `src/app.rs` | Command dispatch, business logic | ~2700 |
| `src/cli.rs` | CLI argument parsing (manual, not clap derive) | ~800 |
| `src/storage.rs` | File I/O, states.toml, archive, SQLite | ~900 |
| `src/mcp.rs` | MCP server tool definitions | ~900 |
| `src/output.rs` | View types (StoryView, SummaryView) | ~100 |
| `src/decompose.rs` | Story decomposition from specs | ~900 |
| `src/github/` | GitHub sync (field_map, mod) | ~900 |
| `src/tui/` | Terminal UI (board, graph, dashboard) | ~2000 |

## Key Patterns to Follow

### 1. States.toml Pattern (for types.toml)

**File:** `src/storage.rs:64-65, 277-344`

```rust
// Path definition
pub fn states_file(&self) -> PathBuf {
    self.storyhook_dir().join("states.toml")
}

// Loading
pub fn load_states(root: &Path) -> Result<Vec<StateDef>, AppError> {
    let raw = fs::read_to_string(paths.states_file())?;
    let states_file = toml::from_str::<StatesFile>(&raw)?;
    validate_state_defs(&states_file.states)?;
    Ok(states_file.states)
}

// CRUD: add_state, remove_state, save_states, default_open_state
```

**Types.toml should mirror this exactly:**
- Add `types_file()` to `ProjectPaths` (storage.rs:64)
- Create `TypeDef { slug: String, description: Option<String> }`
- Create `TypesFile { types: Vec<TypeDef> }` (storage.rs:45 pattern)
- Implement `load_types`, `save_types`, `add_type`, `remove_type`

### 2. Event Pattern (StoryPrioritySet → StoryTypeSet)

**File:** `src/domain.rs:184-187`

```rust
StoryPrioritySet {
    at: String,
    priority: Priority,
},
```

**New event:**
```rust
StoryTypeSet {
    at: String,
    story_type: String,
},
```

**Fold pattern** (`src/domain.rs:302-308`):
```rust
StoryEvent::StoryPrioritySet { at, priority: new_priority } => {
    priority = new_priority.clone();
    updated_at = Some(at.clone());
}
```

### 3. StorySnapshot Extension

**File:** `src/domain.rs:124-145`

Add after `labels`:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub story_type: Option<String>,
```

### 4. CLI Subcommand Pattern

**File:** `src/cli.rs` — Manual arg parsing, NOT clap derive.

Commands are parsed in a large match block. Example pattern from `story list`:
```
"list" => { /* parse --state, --priority, --label flags */ }
```

Phase subcommands follow the pattern (in `src/app.rs`):
```
"phase" => match subcommand {
    "list" => ...,
    "show" => ...,
    "create" => ...,
    "add" => ...,
    "remove" => ...,
}
```

### 5. MCP Tool Definitions

**File:** `src/mcp.rs`

Tools are defined as JSON schema objects. Example fields in `storyhook_create_story`:
- `title`, `priority`, `labels`, `phase` are all optional parameters
- Add `story_type` as an optional parameter

In `storyhook_list_stories`:
- Add `story_type` filter parameter
- Mirror `--state`, `--priority` filter pattern

### 6. Parent-Child Relationships

**File:** `src/domain.rs:401-416, 523+`

- Relationships are stored as events: `StoryRelationshipAdded { relation: "parent-of", other_id: "SH-2" }`
- `inverse_relation("parent-of")` returns `"child-of"` and vice versa
- `derive_family_relationships()` computes transitive relationships (ancestor-of, descendant-of)
- Parent constraint validation at `src/app.rs:2134-2177` — single parent, cycle detection
- Children are identified by filtering relations where `relation == "parent-of"`

### 7. Story Next Logic

**File:** `src/app.rs:2477+`

`story next` uses `is_ready()` from domain.rs to filter stories, then sorts by priority. To skip parents:
- Filter out stories that have any `relation == "parent-of"` in their relationships
- This is a simple addition to the next-story filtering logic

### 8. Test Patterns

**File:** `tests/` directory with 31+ integration test files.

Tests use a helper pattern:
- Create temp directory
- `story init` with a prefix
- Exercise commands
- Assert on JSON output

Example files to follow:
- `tests/priority_test.rs` — for type CRUD tests
- `tests/relationship_test.rs` — for parent-child progress tests
- `tests/decompose_test.rs` — for decompose + type interaction tests

### 9. Archive System

**File:** `src/storage.rs:552-579, 855-885`

- `archive_story()` folds events into snapshot, stores in SQLite as JSON blob
- Schema: `id TEXT PRIMARY KEY, snapshot_json TEXT, events_json TEXT, closed_at TEXT, state TEXT`
- Migration pattern exists: `PRAGMA table_info` check + `ALTER TABLE ADD COLUMN`
- **No schema change needed initially** — type is inside `snapshot_json`

### 10. Doctor/Integrity Checks

**File:** `src/app.rs:2187-2264, src/domain.rs:430+`

- `compute_integrity_issues()` checks relationship consistency
- `doctor_report()` aggregates issues
- `doctor_fix()` repairs relationship mismatches
- Add type validation: warn if `story_type` not in `types.toml`

## Files That Need Modification

| File | Changes |
|------|---------|
| `src/domain.rs` | Add `StoryTypeSet` event, `story_type` to `StorySnapshot`, fold logic, type validation |
| `src/storage.rs` | Add `types_file()`, `load_types()`, `save_types()`, `add_type()`, `remove_type()`, default types |
| `src/app.rs` | Add `type` and `epic` subcommand handlers, `--type` filter, next-story parent filter, progress rollup |
| `src/cli.rs` | Parse `type` and `epic` subcommands, `--type` flag |
| `src/mcp.rs` | Add `story_type` to create/update/list/get tools |
| `src/output.rs` | Add progress info to `StoryView` or `SummaryView` |
| `src/decompose.rs` | Consider `--parent-type` flag |
| `src/github/field_map.rs` | Add type to field map (for future sync) |
| `src/tui/` | Display type badges, progress bars |
| `tests/` | New test files for types and epic functionality |

## Architecture Constraints

1. **No clap derive** — CLI is manually parsed
2. **Event log is append-only** — never modify existing events
3. **Single parent enforcement** — already exists, reuse for epic hierarchy
4. **States are config, not code** — types must follow same principle
5. **JSON output for all commands** — every new command needs `--json` support
6. **MCP tools mirror CLI** — every CLI capability should have an MCP equivalent
