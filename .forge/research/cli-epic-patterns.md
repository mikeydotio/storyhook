# Research: CLI Epic Patterns & Type System UX

## Key Findings

### 1. CLI Flag Conventions for Type Filtering (Confidence: High)

Industry practice across CLI tools:
- **`--type`** is the dominant convention (GitHub CLI: `gh issue list --type`, Jira CLI, Linear CLI)
- `-t` is commonly reserved for `--type` as shorthand
- Consistency with storyhook's existing `--state`, `--priority`, `--label` flags suggests `--type` is natural

**Recommendation:** `--type <slug>` for list filtering, `-t <slug>` as shorthand.

### 2. Epic Subcommand Patterns (Confidence: High)

Two approaches exist in CLI tools:

**A. Namespace subcommands (sugar):**
```
story epic list          # sugar for: story list --type epic
story epic show SH-1     # sugar for: story SH-1 (with epic context)
story epic create "Foo"  # sugar for: story new --type epic "Foo"
story epic add SH-1 SH-2 # sugar for: story relate SH-1 parent-of SH-2
```

**B. Flags only:**
```
story list --type epic
story new --type epic "Foo"
```

**Recommendation:** Both. The `story epic` subcommand is ergonomic sugar that makes discovery easier (`story epic --help`), but all operations are also available via flags. This matches storyhook's existing `story phase` pattern which provides sugar over labels.

The `story phase` subcommand pattern is the direct precedent:
- `story phase list` — lists phases
- `story phase show N` — shows stories in a phase
- `story phase create N "Title"` — creates a story with phase label
- `story phase add N SH-X` — adds phase label to story

### 3. Progress Display in Terminals (Confidence: High)

Best practices for CLI progress rollup:

```
SH-1 [epic] Auth System          ████████░░ 80% (4/5 children done)
SH-5 [epic] Data Pipeline         ██░░░░░░░░ 20% (1/5 children done)
SH-9 [story] Simple task          (no children)
```

Key principles:
- **ASCII block characters** (`█░`) work in all terminals, no color dependency
- **Show fraction alongside percentage** (`4/5`) for precision
- **Omit progress bar for leaf stories** — no children means no rollup
- **Respect `--json` mode** — output structured `{ "progress_pct": 80, "children_done": 4, "children_total": 5 }`
- **Pipe-friendly:** When stdout is not a TTY, omit bar characters, show percentage only

### 4. GitHub Sync Considerations (Confidence: Medium)

GitHub introduced Issue Types in 2024-2025 (currently in beta/limited availability):
- Issue types are organization-level, not per-repository
- API support: `issue_type` field on issues, configurable via organization settings
- Types: Bug, Feature, Task, Epic (organization-configurable)

**Current state:** The GitHub Issue Types API is still maturing. Mapping storyhook types to GitHub types would require:
- Organization-level configuration (not just repo-level)
- Handling type mismatches (storyhook type not in GitHub org types)
- The `field_map.rs` already excludes native relations from sync body — types could follow a similar pattern

**Recommendation:** Defer GitHub type sync to a future release. Store `story_type` in the GitHub issue body metadata (like labels are synced) as a fallback, and add native type sync when the GitHub API stabilizes.

### 5. Type Configuration File Design (Confidence: High)

Following the `states.toml` pattern:

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

Minimal metadata per type:
- `slug` (required) — identifier used in events and filters
- `description` (optional) — shown in `story type list` and help text

**Don't add:** color, icon, emoji, default flag. Keep it simple. Types are classification, not presentation. The TUI can derive colors from a hardcoded palette by index.

### 6. `story decompose` and Auto-Typing (Confidence: Medium)

When `story decompose` creates child stories under a parent:
- The decompose spec already creates `child-of` relationships
- Auto-typing the parent as "epic" is tempting but violates the principle that types are pure classification
- A parent could be a "task" with subtasks, or a "bug" decomposed into fix steps

**Recommendation:** Don't auto-type. Instead, if the parent has no type and decomposition creates children, suggest (via CLI hint) that the user might want to set a type. Let `story decompose --parent-type epic` be an explicit opt-in flag.

## Pitfalls to Avoid

- **Don't conflate type with behavior** — types are labels, not workflow controllers
- **Don't require a type** — existing stories and new `story new "Foo"` should work without specifying one
- **Don't make epic subcommands behave differently from flag equivalents** — they're sugar, not special behavior
- **Don't sync types to GitHub yet** — the API is immature and the sync complexity isn't worth it now

## Open Questions

- Should `story epic remove SH-1 SH-2` be sugar for `story unrelate SH-1 parent-of SH-2`?
- Should `story next --no-parents` be a flag? Or should skip-parents be implicit and `--include-parents` be the override?
- How should types display in the TUI board view? Column headers? Row badges? Color-coded backgrounds?
