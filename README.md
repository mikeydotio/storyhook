# story

`story` is a CLI-first story and issue tracker built for local repositories, scripting, and AI coding agents.

It keeps active work in project-local JSON event logs, archives closed work into SQLite, and favors short commands that are easy to type, pipe, and automate.

## Why `story`

- Local-first: project data lives in `/.storyhook` so it can travel with the repository.
- Agent-friendly: concise commands, stable `--json` output, and explicit exit codes.
- Audit-friendly: open stories are append-only JSONL event streams.
- Safe under concurrent use: all writes use a project-scoped file lock; archived stories live in SQLite with WAL enabled.

## Current capabilities

- Create and show stories
- Add comments with `story <id> "comment"`
- Assign members
- Define project states mapped to `OPEN` or `CLOSED`
- Set and clear `awaiting` blockers
- Set priority levels (critical, high, medium, low, none)
- Add and filter by labels/tags
- Search stories by title, comments, and labels
- Project summary with state/priority breakdown
- Find next ready-to-work stories (`story next`)
- Add and remove story relationships
- Derive read-only `ancestor-of` and `descendent-of` family relationships on show output
- Archive stories immediately when they move into a `CLOSED` state
- Reopen archived stories
- Import/export stories (JSON bulk operations)
- Generate AI context documents (`story context`)
- Session handoff documents (`story handoff`)
- Dependency graph analysis (critical path, blocked chains, parallel groups)
- Configurable project ID prefix
- Run integrity checks with `story doctor` and best-effort repair with `story doctor --fix`

## Install

### Quick install (Linux / macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/mikeydotio/storyhook/main/install.sh | sh
```

This detects your platform and architecture, downloads the latest release binary, and installs it to `~/.local/bin/story`.

To install to a different location:

```bash
STORYHOOK_INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/mikeydotio/storyhook/main/install.sh | sh
```

To install a specific version:

```bash
STORYHOOK_VERSION=v0.2.0 curl -fsSL https://raw.githubusercontent.com/mikeydotio/storyhook/main/install.sh | sh
```

### Install with Cargo

```bash
cargo install storyhook
```

Or from a local checkout:

```bash
cargo install --path .
```

### Prebuilt binaries

Download a release archive from the [releases page](https://github.com/mikeydotio/storyhook/releases) and extract the `story` binary to a directory in your PATH.

Available targets:
- `story-x86_64-unknown-linux-gnu.tar.gz` — Linux x86_64
- `story-aarch64-unknown-linux-gnu.tar.gz` — Linux ARM64
- `story-x86_64-apple-darwin.tar.gz` — macOS Intel
- `story-aarch64-apple-darwin.tar.gz` — macOS Apple Silicon

## Uninstall

If installed via the install script or manual download:

```bash
rm ~/.local/bin/story
```

If installed via Cargo:

```bash
cargo uninstall storyhook
```

## Quick start

Initialize a project inside the repository you want to track:

```bash
story init
```

Create a story:

```bash
story new "Build CLI parser"
```

Add collaborators:

```bash
story member add "Mikey Ward <mw@mikey.io>"
story member add -g mikeyward
```

Work the story:

```bash
story SH-1 assign mikey
story SH-1 "Parser skeleton is in place"
story SH-1 awaits "waiting on command grammar decision"
story SH-1 awaits --clear
story SH-1 is in-progress "Hooked up argument routing"
story SH-1 is done "Merged and verified"
```

Relate stories:

```bash
story SH-1 parent-of SH-2
story SH-2 precedes SH-3
story SH-4 conflicts-with SH-5
story SH-2 parent-of SH-3 --remove
```

Prioritize, label, and triage:

```bash
story SH-1 priority high
story SH-1 label backend,api
story next
story summary
story context
```

Inspect and report:

```bash
story SH-1
story list --state todo
story list --assignee mikey
story list --flagged
story doctor
```

## Command reference

```text
story init [--prefix <PREFIX>]
story new <title>
story member add "<name <email>>"
story member add -g <github-handle>
story state add <state-slug> --super OPEN|CLOSED
story state remove <state-slug>
story list [--state <slug>] [--assignee <id|handle>] [--flagged] [--priority <levels>]
           [--label <labels>] [--created-after <date>] [--updated-after <date>]
           [--blocked] [--ready]
story next [--count <n>]
story summary
story search <query>
story import [<file>]
story export
story import-project <file>
story context [--format markdown|json]
story handoff [--since <duration>]
story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]
story doctor [--fix]
story <id>
story <id> "<comment>"
story <id> assign <member-id|handle>
story <id> is <state-slug> ["<comment>"]
story <id> awaits "<reason>"
story <id> awaits --clear
story <id> priority <critical|high|medium|low|none>
story <id> label <labels-csv>
story <id> label --remove <labels-csv>
story <id> reopen
story <a> <relationship> <b> [--remove]
```

## States

- Every project state maps to exactly one superstate: `OPEN` or `CLOSED`.
- A project must have at least one `OPEN` state and at least one `CLOSED` state.
- New projects start with `todo -> OPEN` and `done -> CLOSED`.
- Moving a story into a `CLOSED` state immediately archives it to SQLite.
- Closed stories remain readable but are no longer mutable.

## Relationships

Supported direct relationship inputs:

- `starts-before` / `starts-after`
- `starts-with`
- `finishes-before` / `finishes-after`
- `finishes-with`
- `precedes` / `follows`
- `relieves` / `relieved-by`
- `conflicts-with`
- `coincides-with`
- `parent-of` / `child-of`
- `relates-to`
- `obviates` / `obviated-by`

Derived, read-only relationships shown on story views:

- `ancestor-of`
- `descendent-of`

Notes:

- Directional relationships automatically create their inverse on the related story.
- Mutual relationships create matching links on both stories.
- `parent-of` implies scheduling edges and enforces a single-parent rule for the child.
- New parent/child links that would create a cycle are rejected.

## Storage model

Project data lives in `/.storyhook`:

```text
.storyhook/
  project.toml
  states.toml
  members.jsonl
  next-id
  lock
  open/
    stories/
      SH-1.jsonl
  archive/
    archive.db
    archive.db-wal
```

Behavior:

- Open stories are stored as append-only JSONL event streams.
- Closed stories are archived into SQLite.
- The story ID counter is project-local and monotonic: `SH-1`, `SH-2`, `SH-3`, ...
- Every write acquires a project-scoped file lock before mutating state.

## Automation and scripting

`story` is designed to be used by shell scripts and coding agents.

Global flags:

- `--json` emits a structured JSON response envelope
- `--quiet` suppresses normal success output

Exit codes:

- `0` success
- `2` usage or validation error
- `3` not found
- `4` lock timeout
- `5` integrity or storage error

Examples:

```bash
story SH-1 --json
story list --flagged --json
story SH-2 is done --quiet
```

## AI agent integration

Three commands support AI coding agent workflows:

- `story context` -- generates a project overview document (states, priorities, relationships, and ready work) suitable for the start of an AI session. Use `--format json` for structured output.
- `story next` -- surfaces the highest-priority unblocked story so an agent can pick up work without manual triage. Use `--count <n>` to get multiple candidates.
- `story handoff --since <duration>` -- generates a session handoff document summarizing what changed during a work session (e.g. `--since 2h`). Useful when passing context between agents or between an agent and a human.

## Integrity checks

`story doctor` reports integrity problems such as:

- dangling relationships
- missing inverse relationships
- hierarchy cycles
- duplicate open/archive presence
- invalid multi-parent hierarchies

`story doctor --fix` currently performs best-effort repair for supported issues, including:

- adding missing inverse relationships
- removing dangling direct relationships

## Development

Run the standard checks:

```bash
cargo test
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
```

## Project status

The current release is usable for local, repository-backed tracking and automation workflows. The command surface is intentionally small and stable, and the storage model is built to evolve without abandoning existing project data.
