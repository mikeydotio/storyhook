# Story Types & Epics for Storyhook

## Vision
Add a configurable story type system to storyhook (epic, bug, chore, task, etc.) with epics as the flagship type — enabling cross-phase grouping, progress rollup, and better organizational hierarchy for users managing non-trivial projects.

## Problem Statement
Storyhook has parent-child relationships and phase-based grouping, but no semantic way to distinguish between "this story has subtasks" and "this is a large initiative with stories under it." Users need:
1. **Cross-phase grouping** — related stories spanning multiple phases need a single trackable container (phases are temporal; epics are thematic)
2. **Progress visibility** — no way to see aggregated completion stats for a body of work
3. **Story classification** — no way to distinguish bugs from features from chores in the data model (currently only possible via labels)

## Target Users
Storyhook users — both the creator (dogfooding) and the growing user base requesting epic support as a first-class feature.

## Key Requirements
- [ ] Configurable story type system via `types.toml` (mirrors `states.toml` pattern)
- [ ] Ship with sensible default types: story, epic, bug, chore, task
- [ ] New `StoryTypeSet` event in the event-sourcing model
- [ ] `story type add/remove/list` CLI commands for managing types
- [ ] `story new --type epic "Title"` to create typed stories
- [ ] `story set <id> --type epic` to change a story's type
- [ ] `story epic list` — list all epics with progress rollup
- [ ] `story epic show <id>` — epic detail with child progress breakdown
- [ ] `story epic create "Title"` — sugar for `story new --type epic`
- [ ] `story epic add <epic-id> <story-id>` — sugar for adding parent-of relationship
- [ ] Universal progress rollup: any story with children shows completion % based on immediate children
- [ ] `story next` skips stories that have children by default
- [ ] `story list --type <type>` filter support
- [ ] MCP tools updated: type field in create/update/list/get operations
- [ ] `story summary` and `story context` surface epic/type information
- [ ] `story doctor` checks for type integrity (e.g., warn on unknown types)
- [ ] Migration path for existing projects (add types.toml with defaults on first use)

## Assumptions (Examined)
| Assumption | Challenged? | Status |
|-----------|------------|--------|
| Epics should be a story type, not a separate entity | Yes — compared Jira (separate entity) vs GitHub (issue with type). User confirmed: "Epic should be a story type like Bug or Chore" | Validated |
| Types should be configurable, not hardcoded | Yes — presented fixed vs configurable options. User chose configurable, consistent with states pattern | Validated |
| Progress rollup should be universal for any parent, not just epics | Yes — user clarified: "any given story's completion should be as a percentage of its immediate children that are complete, unless it has no children" | Validated |
| story next should skip parents by default | Yes — presented three options. User confirmed skip-parents approach | Validated |
| Direct children only for progress (not recursive) | Yes — presented direct vs recursive vs both. User chose direct children | Validated |
| Types are purely classification (no behavioral roles) | Partially — user chose configurable types without roles. Types don't drive behavior; parenthood does (progress rollup, next filtering) | Validated |

## Constraints
- Must be backward-compatible — existing projects without `types.toml` continue to work
- Types are stored as a configurable list, not hardcoded in the binary
- No separate storage for epics — they use the same JSONL event logs as all stories
- Same ID namespace — epics are SH-N, not EP-N
- Progress rollup is computed on read, never stored
- Must update both CLI and MCP server interfaces

## What "Done" Looks Like
1. `story new --type epic "Auth System"` creates a story typed as epic
2. `story epic list` shows all epics with per-epic progress bars
3. `story epic show SH-1` shows the epic with child state breakdown
4. `story next` returns actionable leaf work, not parent containers
5. `story list --type bug` filters to bugs
6. `story type add spike` adds a custom type
7. Existing projects auto-migrate to include types.toml on first interaction
8. MCP tools expose type field for AI agent workflows
9. `story summary` includes type breakdown
10. All existing tests continue to pass; new tests cover type and epic functionality

## Open Questions
- Should `story decompose` auto-assign the type:epic to parent stories in decomposition specs?
- How should types interact with GitHub sync? Map to GitHub issue types?
- Should there be a `--no-parents` flag on `story next` (to make the skip-parents behavior explicit and overridable)?
- Should `story epic remove <epic-id> <story-id>` be sugar for removing parent-of relationship?
- How should types display in the TUI board view?
- Should archived (closed) children count toward epic progress?

## Prior Art
- **GitHub Issues (2025)**: Sub-issues + issue types. Everything is an issue; type is classification metadata. Closest to our approach.
- **Jira**: Epics as separate issue type with own workflow. Industry is moving away from this.
- **Linear**: Rejects "epic" concept; uses Projects as grouping. Flat model.
- **GitLab**: Migrating epics from separate entity to work item type (validating our approach).
- **Shortcut**: Dedicated epic entity with own workflow states. More ceremony.
- **Taskwarrior/dstask**: No formal epic support. CLI tools haven't solved this well.

The industry trend strongly validates "epic = typed story with children" over "epic = separate entity."
