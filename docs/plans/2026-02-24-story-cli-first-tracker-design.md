# Story Design: CLI-First Story/Issue Tracker for AI Agents

Date: 2026-02-24  
Status: Approved

## 1. Goals

- Build an OSS, self-hostable story/issue tracker designed for CLI-first automation.
- Optimize for low-friction use by AI coding agents and scripts.
- Guarantee safe behavior under concurrent access by multiple agents.
- Keep project data in the project folder for repository portability.

## 2. Core Product Decisions

- Binary name: `story`
- Runtime/language: Rust
- Story IDs: monotonic project-local keys (`SH-1`, `SH-2`, ...)
- Fixed superstates: `OPEN`, `CLOSED`
- Project must always have at least one state mapped to `OPEN` and at least one mapped to `CLOSED`
- Storage split:
  - OPEN stories: JSON event logs in-project
  - CLOSED stories: SQLite archive with WAL and locking
- Close behavior: immediate atomic archive when a story transitions into a CLOSED-mapped state
- Concurrency model for writes: single project writer lock
- Output mode: human-readable by default, `--json` for structured output

## 3. Architecture

Layered design:

- `cli`: argument parsing, dispatch, output formatting
- `application`: command handlers and orchestration
- `domain`: entities, invariants, relationship/state rules
- `storage_open`: JSONL event streams + indexes
- `storage_archive`: SQLite archive schema and transactions
- `locking`: project-scoped write lock

Project data layout under `/.storyhook`:

- `project.toml`
- `states.toml`
- `members.jsonl`
- `next-id`
- `open/stories/SH-<n>.jsonl`
- `open/indexes/*.json`
- `archive/archive.db` (+ WAL side files)

## 4. Domain Model

### 4.1 Entities

- `Project`
- `Member`
- `StateDef` (`slug`, `superstate`)
- `Story`

A story has:

- id, title, created_at, updated_at
- assignee (nullable)
- state (project-defined state slug)
- awaiting (nullable string)
- comments (chronological)
- relationships
- change history (append-only events)

### 4.2 Status Traits

- `state`: required
- `awaiting`: optional
- `flagged`: computed

A story is flagged when:

- it has `obviated-by`
- it has `conflicts-with`
- it has illegal graph/state conditions (cycle, orphan edge, invalid inverse, etc.)

### 4.3 Relationships

Directional relationships auto-create inverse edges. Mutual relationships are symmetric.

Supported relationships:

- `starts-before` / `starts-after`
- `starts-with`
- `finishes-before` / `finishes-after`
- `finishes-with`
- `precedes` / `follows`
- `relieves` / `relieved-by`
- `conflicts-with`
- `coincides-with`
- `parent-of` / `child-of`
- `ancestor-of` / `descendent-of` (virtual/derived)
- `relates-to`
- `obviates` / `obviated-by`

Constraints:

- child may have only one parent
- parent may have many children
- no parent/child cycles
- `parent-of` implies scheduling semantics (`starts-before`, `finishes-after`)
- `child-of` implies inverse scheduling semantics
- `coincides-with` semantically combines start+finish coupling

## 5. Data Flow and Concurrency

### 5.1 Write Commands

For all mutating commands:

1. Acquire project write lock
2. Load minimal metadata/index scope
3. Validate invariants
4. Append events to JSONL streams
5. Update impacted indexes
6. Release lock

### 5.2 Read Commands

- Use indexes first, then fold relevant streams as needed
- No write lock required for reads

### 5.3 Close and Archive Path (Atomic)

When state changes to a CLOSED-mapped state:

1. Acquire project lock
2. Validate transition
3. Append close-related JSON events
4. Open SQLite transaction (`WAL`)
5. Persist final snapshot + comments + relationships + history payload/reference
6. Mark open stream/index record archived
7. Commit transaction and release lock

Recovery guarantees:

- Incomplete archive attempts are detectable and repairable idempotently
- No successful command leaves split-brain between OPEN and archive stores

## 6. CLI UX and Command Surface (MVP)

### 6.1 Concise Story Commands

```bash
story <id>                                   # show story
story <id> "<comment>"                       # add comment
story <id> assign <member-id|handle>         # set assignee
story <id> is <state-slug> ["<comment>"]     # change state + optional comment
story <a> <relationship> <b> [--remove]      # add/remove relationship
```

Parser precedence:

1. `story <id> assign ...`
2. `story <id> is ...`
3. relationship form where token 2 is a known relationship type
4. `story <id>` (show)
5. `story <id> "<comment>"` (comment)

### 6.2 Supporting Commands

```bash
story init
story member add "<name <email>>"
story member add -g <github-handle>
story state add <state-slug> --super OPEN|CLOSED
story state remove <state-slug>
story new "<title>"
story list [--state <slug>] [--assignee <id|handle>] [--flagged]
story doctor [--fix]
```

### 6.3 Automation-Focused Behavior

- default concise human output
- `--json` stable machine schema
- `--quiet` status-only scripting mode

Exit code baseline:

- `0` success
- `2` validation/domain error
- `3` not found
- `4` lock contention/timeout
- `5` storage/integrity error

## 7. Integrity and Error Handling

- Domain errors are explicit and actionable
- Relationship updates are idempotent where possible
- `story doctor` checks and reports:
  - orphan/dangling edges
  - missing inverses
  - asymmetry in mutual links
  - hierarchy cycles or multi-parent violations
  - open/archive boundary mismatches
- `story doctor --fix` performs explicit, logged repairs

## 8. Testing Strategy

Target: at least 90% coverage for domain + application logic.

Test tiers:

- Unit tests:
  - state mapping and transitions
  - relationship inverse/symmetry behavior
  - parent constraints and cycle detection
  - flagged computation
- Integration tests:
  - CLI parsing/dispatch for concise command forms
  - end-to-end storage mutations across JSONL + indexes
  - close/archive atomic path
- Concurrency tests:
  - parallel writers under lock contention
  - deterministic event ordering
- Failure injection tests:
  - forced failure around archive boundary, verified recoverability

## 9. Out of Scope for MVP

- User authentication/authorization
- Distributed multi-node coordination
- Non-CLI UI surfaces

## 10. Next Step

Proceed to implementation planning using `writing-plans` skill, based on this approved design.
