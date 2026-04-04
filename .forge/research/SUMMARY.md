# Research Summary

## Key Findings

1. **Additive event evolution is the right pattern.** Adding `StoryTypeSet` as a new event variant (mirroring `StoryPrioritySet`) is a non-breaking, forward-only change. Existing event logs need zero migration. The `fold_story` function gets a new match arm; stories without the event default to `story_type: None`.

2. **No archive migration needed.** The SQLite archive stores snapshots as JSON blobs (`snapshot_json TEXT`). Adding `story_type: Option<String>` with `#[serde(default)]` to `StorySnapshot` means old snapshots deserialize with `None` automatically. A denormalized column can be added later if type-based archive queries become needed (using the existing `PRAGMA table_info` migration pattern).

3. **Types must be strings, not enums.** Since types are user-configurable via `types.toml`, they're stored as `String` validated at write time against the config — exactly like states. The `types.toml` format should mirror `states.toml` with `[[types]]` sections containing `slug` and optional `description`.

4. **Storyhook already has all the building blocks.** The codebase has clear, repeatable patterns: `states.toml` for type config, `StoryPrioritySet` for the event, `story phase` for the subcommand sugar, parent-child relationships for hierarchy, and `story doctor` for integrity checks. Implementation is primarily pattern-following, not novel architecture.

5. **Progress rollup is a read-time computation.** For any story with `parent-of` relationships, compute `children_done / children_total` by checking which children have `superstate == Closed`. Never store progress — derive it from child states. This keeps the event model clean.

## Existing Solutions

No existing tool matches storyhook's exact approach (event-sourced CLI with configurable types). However, the design is validated by industry trends:

- **GitHub Issues (2025):** Moved to "everything is an issue, type is metadata" — exactly our approach
- **GitLab:** Migrating epics from separate entities to work item types — validating our direction  
- **Jira:** Industry moving away from Jira's "epic as separate entity" model
- **Taskwarrior/dstask:** No formal epic support — storyhook would be first in CLI space

## Recommended Technology Stack

No new dependencies needed. The implementation uses:
- **Rust + serde** for type serialization (existing)
- **TOML** for types.toml config (existing `toml` crate)
- **SQLite** for archive (existing `rusqlite` crate)
- **JSONL** for event storage (existing)

## Patterns to Follow

| Pattern | Source | Apply To |
|---------|--------|----------|
| `states.toml` config loading | `storage.rs:277-344` | `types.toml` loading |
| `StoryPrioritySet` event | `domain.rs:184-187` | `StoryTypeSet` event |
| `fold_story` event handling | `domain.rs:302-308` | Type field in fold |
| `story phase` subcommands | `app.rs` phase handlers | `story type` and `story epic` subcommands |
| `--state`/`--priority` list filters | `cli.rs:507`, `app.rs` | `--type` list filter |
| `PRAGMA table_info` migration | `storage.rs:871-882` | Future archive column |
| `doctor_report` integrity | `app.rs:2187-2204` | Type validation check |

## Pitfalls to Avoid

1. **Don't modify `StoryCreated` event** — add `StoryTypeSet` as separate event for backward compatibility
2. **Don't validate types during event fold** — fold reconstructs state; validation happens at command time against `types.toml`
3. **Don't force default types on existing stories** — `None` means "untyped" and is semantically correct
4. **Don't auto-type parents as epics** — type is classification, behavior is driven by parenthood
5. **Don't sync types to GitHub yet** — Issue Types API is immature; defer to future release
6. **Don't add color/icon/emoji to TypeDef** — keep it minimal (slug + description), like StateDef

## Open Questions

1. Should `story epic remove SH-1 SH-2` exist as sugar for `story unrelate SH-1 parent-of SH-2`?
2. Should `story next` have `--no-parents` (explicit) or just always skip parents?
3. How should types display in the TUI board view?
4. Should archived (closed) children count toward parent progress? (Recommendation: yes — CLOSED is CLOSED regardless of storage location)
5. Should `story decompose` accept `--parent-type epic` flag?
