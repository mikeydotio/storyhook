# Research: Event-Sourced Type Systems

## Key Findings

### 1. Adding a New Field to an Event-Sourced Model (Confidence: High)

The standard approach for adding a new field (like `story_type`) to an event-sourced model is **additive event evolution**:

- **Add a new event variant** (`StoryTypeSet`) rather than modifying existing events
- Existing events (e.g., `StoryCreated`) remain unchanged — no migration needed for the event log
- During `fold_story`, if no `StoryTypeSet` event exists, the field defaults to `None` (or an implicit default like `"story"`)
- This is a **non-breaking, forward-only change** — the gold standard for event-sourced schema evolution

Storyhook already follows this pattern: `StoryPrioritySet` was added without changing `StoryCreated`, and `priority` defaults to `Priority::None` via `#[serde(default)]`.

### 2. Archive (SQLite) Schema Migration (Confidence: High)

The archive stores `snapshot_json` and `events_json` as TEXT columns. The type field will automatically appear in `snapshot_json` for newly archived stories because it becomes part of `StorySnapshot`.

For **existing archived stories**, `serde(default)` on the `story_type` field in `StorySnapshot` handles deserialization gracefully — old JSON without the field will deserialize with the default value (`None` or `"story"`).

**No SQLite schema migration needed** — the type is stored inside the JSON blob, not as a separate column. This is a significant advantage of the JSON-snapshot pattern.

However, if type-based queries on the archive are desired (e.g., `story list --type epic` searching closed stories), a denormalized column could be added later using the existing migration pattern:

```rust
// Existing pattern in open_archive_connection():
let has_col: bool = connection
    .prepare("PRAGMA table_info(closed_stories)")?
    .query_map([], |row| row.get::<_, String>(1))?
    .filter_map(|r| r.ok())
    .any(|name| name == "story_type");
if !has_col {
    connection.execute(
        "ALTER TABLE closed_stories ADD COLUMN story_type TEXT",
        [],
    )?;
}
```

### 3. Type Representation: String vs Enum (Confidence: High)

Since types are **user-configurable** via `types.toml`, they must be stored as `String`, not a Rust enum. This mirrors how `state` is a `String` in `StorySnapshot` validated against `states.toml` at runtime.

The `StorySnapshot` field should be:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub story_type: Option<String>,
```

Using `Option<String>` rather than `String` preserves backward compatibility — existing stories without a type deserialize to `None`.

### 4. Type Validation During Event Replay (Confidence: High)

When replaying events and encountering a `StoryTypeSet` with a type that no longer exists in `types.toml`:

**Recommended: Preserve the value, flag during doctor checks.**

- `fold_story` should NOT validate types — it reconstructs state, not policy
- `story doctor` should flag stories whose type isn't in `types.toml` (soft warning)
- `story doctor --fix` could clear invalid types or prompt for remapping
- This matches how storyhook handles states — `fold_story` doesn't validate state slugs

### 5. Default Type Strategy (Confidence: High)

**Recommended: `Option<String>` with `None` meaning "untyped".**

- Stories created before the type feature have no type — they shouldn't be retroactively assigned one
- `None` is semantically correct: "this story has no type classification"
- Users can filter untyped stories with `story list --type none` or similar
- The `story new` command creates untyped stories unless `--type` is specified
- Default types in `types.toml` should include a "story" type, but it's not auto-assigned

This avoids the problem of implicit defaults changing behavior for existing projects.

## Recommendations

1. **Add `StoryTypeSet { at: String, story_type: String }` event variant** — follows `StoryPrioritySet` pattern exactly
2. **Add `story_type: Option<String>` to `StorySnapshot`** with `#[serde(default)]`
3. **Validate type against `types.toml` at write time** (when setting type), not at replay time
4. **No archive migration needed** initially — JSON blobs handle it
5. **Consider `StoryTypeCleared { at: String }` event** for removing a type (setting back to None)

## Pitfalls to Avoid

- **Don't add type to `StoryCreated`** — this breaks backward compatibility for all existing event logs
- **Don't validate types during fold** — it couples event replay to config state, which can break if types.toml changes
- **Don't use a Rust enum for types** — they're user-configurable, not a fixed set
- **Don't force a default type on existing stories** — respect the migration path

## Open Questions

- Should there be a `story_type` denormalized column in the archive DB for query performance? (Probably not needed initially)
- Should `story decompose` auto-set the parent's type to "epic"? (UX question, not a technical one)
