# Storyhook CLI Reference

Complete command reference for the `story` CLI. All commands operate on the `.storyhook/` directory in the current working directory.

## Global Flags

These flags can be added to any command:

| Flag | Description |
|---|---|
| `--json` | Emit structured JSON output instead of human-readable text |
| `--quiet` | Suppress success output (still shows errors) |
| `--no-hooks` | Suppress event hook execution for this command |
| `-h`, `--help` | Show usage help |

---

## Project Commands

### `story init [--prefix <PREFIX>]`

Initialize a new storyhook project in the current directory.

- Creates `.storyhook/` directory with `project.toml`, `states.toml`, `next-id`, and `open/` folder
- Default prefix is auto-detected from directory name (uppercase, truncated)
- Use `--prefix` to set a custom story ID prefix (e.g., `--prefix API` for `API-1`, `API-2`, ...)
- **Use when**: starting a new project or adding storyhook to an existing repo
- **Do not use when**: `.storyhook/` already exists (will error)

```bash
story init
story init --prefix SH
```

### `story summary`

Show a compact summary of project state: total stories, counts by state, ready count, blocked count.

- **Use when**: you need a quick overview of where things stand
- **Related**: `story context` (more detailed), `story list` (individual stories)

```bash
story summary
story summary --json
```

### `story report [--html]`

Generate a detailed project report.

- Without `--html`: outputs a markdown report
- With `--html`: outputs an HTML report suitable for sharing
- **Use when**: preparing a status update or project review

```bash
story report
story report --html > report.html
```

### `story context [--format markdown|json]`

Show a comprehensive project overview including open stories, priorities, blockers, and recommended next work.

- Default format is markdown (human-readable)
- **Use when**: starting a session and need full project context
- **Related**: `story summary` (compact), `story next` (just the recommendation)

```bash
story context
story context --format json
```

### `story doctor [--fix]`

Diagnose project health issues: orphaned files, invalid states, broken relationships.

- Without `--fix`: reports problems only
- With `--fix`: attempts to automatically repair issues
- **Use when**: something seems wrong, stories are missing, or after manual edits to `.storyhook/`

```bash
story doctor
story doctor --fix
```

---

## Story Creation and Search

### `story new <title>`

Create a new story with the given title. Returns the new story ID.

- Title can be quoted or unquoted (multiple words are joined)
- New stories start in the first OPEN state (usually `todo`)
- **Use when**: adding a new task, bug, or feature to track

```bash
story new "Implement user authentication"
story new Fix login page redirect bug
```

### `story search <query>`

Full-text search across story titles and comments.

- Searches all open stories
- **Use when**: looking for a specific story by keyword

```bash
story search "authentication"
story search login bug
```

### `story list [options]`

List open stories with optional filters.

| Filter | Description |
|---|---|
| `--state <slug>` | Filter by state (e.g., `todo`, `in-progress`, `done`) |
| `--assignee <id\|handle>` | Filter by assigned member |
| `--flagged` | Show only flagged stories |
| `--priority <levels>` | Filter by priority (comma-separated: `critical,high`) |
| `--label <labels>` | Filter by labels (comma-separated) |
| `--created-after <date>` | Stories created after date (ISO 8601) |
| `--updated-after <date>` | Stories updated after date (ISO 8601) |
| `--blocked` | Show only blocked stories (have `awaits` set) |
| `--ready` | Show only ready stories (no blockers, all deps met) |
| `--stale <duration>` | Stories not updated within duration (e.g., `3d`, `1w`) |

- **Use when**: filtering the backlog by specific criteria
- **Do not use when**: you just need the next task (use `story next` instead)

```bash
story list
story list --state in-progress
story list --blocked --json
story list --priority critical,high
story list --stale 7d
story list --ready --json
```

### `story next [--count <n>]`

Get the top recommended story to work on next, considering priority, dependencies, and staleness.

- Default count is 1
- Only returns stories that are ready (not blocked, dependencies met)
- **Use when**: picking what to work on next
- **Related**: `story list --ready` (all ready stories without ranking)

```bash
story next
story next --count 5 --json
```

---

## Story Operations

### `story <id>`

Show full details for a story: title, state, priority, labels, comments, relationships.

```bash
story SH-3
story SH-3 --json
```

### `story <id> "<comment>"`

Add a comment to a story.

- Use for progress notes, decisions, blockers, or any context worth recording
- **Use when**: logging what you did, what you learned, or what's next

```bash
story SH-3 "Implemented the database schema migration"
story SH-3 "Blocked: waiting on API spec from team"
```

### `story <id> is <state-slug> ["<comment>"]`

Transition a story to a new state, optionally with a comment.

- State must be defined in `.storyhook/states.toml`
- Default states: `todo` (OPEN), `done` (CLOSED)
- Projects may add custom states like `in-progress`, `review`, etc.
- **Use when**: starting, completing, or changing the status of work

```bash
story SH-3 is in-progress
story SH-3 is done "All tests passing, feature complete"
story SH-3 is review "Ready for code review"
```

### `story <id> assign <member-id|handle>`

Assign a story to a team member.

- Member must be registered with `story member add`
- **Use when**: designating who is responsible for a story

```bash
story SH-3 assign alice
story SH-3 assign @octocat
```

### `story <id> priority <level>`

Set the priority of a story. Levels: `critical`, `high`, `medium`, `low`, `none`.

- Priority affects `story next` ranking
- **Use when**: adjusting task importance during triage or planning

```bash
story SH-3 priority high
story SH-3 priority critical
```

### `story <id> label <labels-csv>`

Add labels to a story (comma-separated).

```bash
story SH-3 label backend,auth
story SH-3 label bug,urgent
```

### `story <id> label --remove <labels-csv>`

Remove labels from a story.

```bash
story SH-3 label --remove backend
```

### `story <id> awaits "<reason>"`

Mark a story as blocked with a reason.

- Blocked stories are excluded from `story next` results
- **Use when**: work cannot proceed until something external happens

```bash
story SH-3 awaits "Waiting for API design review"
story SH-3 awaits "Depends on SH-1 completion"
```

### `story <id> awaits --clear`

Clear the blocked status of a story.

- **Use when**: the blocker has been resolved

```bash
story SH-3 awaits --clear
```

### `story <id> reopen`

Reopen a closed story, moving it back to the first OPEN state.

- **Use when**: a completed story needs more work or was closed prematurely

```bash
story SH-3 reopen
```

---

## Relationships

### `story <a> <relationship> <b> [--remove]`

Create or remove a relationship between two stories. Inverse relationships are automatically maintained.

| Relationship | Meaning | Inverse |
|---|---|---|
| `precedes` | A must finish before B starts | `follows` |
| `follows` | A starts after B finishes | `precedes` |
| `parent-of` | A is a parent/epic containing B | `child-of` |
| `child-of` | A is a sub-task of B | `parent-of` |
| `starts-before` | A starts before B | `starts-after` |
| `starts-after` | A starts after B | `starts-before` |
| `starts-with` | A and B start together | `starts-with` |
| `finishes-before` | A finishes before B | `finishes-after` |
| `finishes-after` | A finishes after B | `finishes-before` |
| `finishes-with` | A and B finish together | `finishes-with` |
| `coincides-with` | A and B start and finish together | (compound) |
| `conflicts-with` | A and B cannot proceed simultaneously | `conflicts-with` |
| `relates-to` | A is related to B (informational) | `relates-to` |
| `obviates` | Completing A makes B unnecessary | `obviated-by` |
| `relieves` | A relieves a constraint on B | `relieved-by` |

- **Use when**: defining task ordering, grouping, or conflicts
- Use `--remove` to delete an existing relationship

```bash
story SH-1 precedes SH-2
story SH-3 parent-of SH-4
story SH-5 relates-to SH-6
story SH-7 obviates SH-8
story SH-1 precedes SH-2 --remove
```

---

## Planning and Decomposition

### `story decompose <file> [--dry-run]`

Parse a markdown or YAML spec file and create stories with relationships and priorities.

- `--dry-run`: preview what would be created without making changes
- `--stdin`: read spec from stdin instead of a file
- **Use when**: breaking down a feature spec or project plan into trackable stories

```bash
story decompose spec.md --dry-run
story decompose spec.md
story decompose plan.yaml --dry-run
echo "# Feature: Auth" | story decompose --stdin --dry-run
```

### `story import-project <file>`

Import an entire project definition from a structured file.

- **Use when**: migrating from another tool or bootstrapping a project from a template

```bash
story import-project project-export.json
```

### `story import [<file>]`

Import stories from a JSON file or stdin.

```bash
story import stories.json
cat stories.json | story import
```

### `story export`

Export all stories as JSON.

```bash
story export > backup.json
story export --json
```

---

## Dependency Graph

### `story graph [options]`

Visualize and analyze story dependencies.

| Mode | Description |
|---|---|
| (no flag) | Show full dependency graph overview |
| `--critical-path` | Show the longest dependency chain (bottleneck) |
| `--blocked-by <id>` | Show all stories transitively blocked by a specific story |
| `--parallel-groups` | Show groups of stories that can be worked on in parallel |

- **Use when**: understanding task ordering, finding bottlenecks, or planning parallel work
- **Related**: `story list --blocked` (just blocked stories)

```bash
story graph
story graph --critical-path
story graph --blocked-by SH-1
story graph --parallel-groups
story graph --json
```

---

## Session and Sync

### `story handoff [--since <duration>]`

Generate a session handoff document summarizing recent activity.

- Default duration: covers the current session (usually `2h`)
- Includes: state changes, comments added, commits linked, current blockers
- **Use when**: ending a work session or switching context

```bash
story handoff
story handoff --since 4h
story handoff --since 1d
```

### `story sync-git [--since <duration>]`

Scan git history and link commits to stories based on story ID references in commit messages.

- Default: scans recent history
- Auto-closes stories mentioned in merge commits with closing keywords
- **Use when**: after pulling, merging, or rebasing to update story state from git

```bash
story sync-git
story sync-git --since 7d
story sync-git --since 30d
```

---

## Configuration and Setup

### `story member add "<name <email>>" | story member add -g <github-handle>`

Register a team member for story assignment.

```bash
story member add "Alice Smith <alice@example.com>"
story member add -g octocat
```

### `story state add <slug> --super OPEN|CLOSED [--role active]`

Add a custom workflow state.

- `--super OPEN` or `--super CLOSED`: whether this state counts as open or closed
- `--role active`: marks this as the "in-progress" state for automation

```bash
story state add in-progress --super OPEN --role active
story state add review --super OPEN
story state add wont-fix --super CLOSED
```

### `story state remove <slug>`

Remove a custom workflow state. Stories in that state will need to be transitioned first.

```bash
story state remove review
```

### `story hooks install|uninstall|list|test <event_type>`

Manage git hooks for automatic story syncing.

- `install`: set up post-commit and other git hooks
- `uninstall`: remove storyhook git hooks
- `list`: show installed hooks
- `test <event_type>`: test-fire a hook event

```bash
story hooks install
story hooks list
story hooks test post-commit
story hooks uninstall
```

### `story scaffold agents-md|claude-md|cursor-rules`

Generate agent instruction templates for integration with AI coding tools.

- `agents-md`: generate `.agents.md` instructions
- `claude-md`: generate `CLAUDE.md` instructions for Claude Code
- `cursor-rules`: generate `.cursorrules` instructions

```bash
story scaffold claude-md
story scaffold agents-md
story scaffold cursor-rules
```
