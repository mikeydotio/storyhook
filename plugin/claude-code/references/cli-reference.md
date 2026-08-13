# Storyhook CLI Reference

Complete command reference for the `story` CLI. Story data lives in a single store outside your repositories; a checkout says which project it belongs to with a committed `.storyhook.toml` at its root. Commands resolve that project from the working directory or its nearest ancestor, so every checkout of a repository — worktrees included — reads and writes the same stories.

**The grammar is strictly verb-first**: every invocation starts with a command word (`move`, `comment`, `set`, `relate`, ...) followed by its arguments. There is no bare-id form — `story SH-3 is done` and `story SH-3 "some text"` are **not valid syntax** and fail with `unknown command` (exit 2). If you're unsure a command exists, run `story help --all` or `story help <command>` against the live binary rather than guessing.

## Global Flags

These flags can be added to any command, before or after the subcommand and its arguments:

| Flag | Description |
|---|---|
| `--json` | Emit structured JSON output instead of human-readable text |
| `--quiet` | Suppress success output (still shows errors) |
| `--no-hooks` | Suppress event hook execution for this command |
| `-h`, `--help` | Show usage help |

```bash
story list --json
story new "Fix login redirect" --json
```

---

## Project Setup

### `story project new --prefix <PREFIX> [--name <NAME>] [--attach <PATH> | --no-attach] [--no-agents-md]`

Create a new storyhook project.

- **`--prefix` is required for you.** With *no* flags at a terminal the command asks six questions instead; any flag at all — or `--json`, or no terminal, which is every agent — makes it fully non-interactive, and then a missing `--prefix` is a usage error. It is minted into every story id the project ever creates and cannot be changed afterwards, so it is never defaulted.
- **No positional.** `--attach` names the checkout and defaults to the directory you are standing in; a relative path resolves against that directory. `--no-attach` writes the store record and touches no directory at all — the shape for a project whose repository is on another machine.
- Creates the project in the store, writes `.storyhook.toml` (commit it — a clone needs it to know which project it is), generates an `AGENTS.md`, and seeds the states every project must have: `todo` (OPEN), `in-progress` (OPEN, `role active`), `blocked` (OPEN), `done` (CLOSED)
- **Use when**: starting a new project or adding storyhook to an existing repo
- **Idempotent**: run in a checkout that already belongs to a project, it re-registers the checkout and leaves the catalog and the prefix alone
- **Do not use when**: the repository still keeps its stories in a `.storyhook/` directory — `new` refuses and points at `story migrate`, because creating one would mint an empty second project beside data you still have

```bash
story project new --prefix SH      # stories become SH-1, SH-2, ...
story project new --prefix HP      # stories become HP-1, HP-2, ...
story project new --prefix TH --attach ~/code/thing
story project new --prefix OFF --no-attach --name "on another machine"
```

### `story project list`

Every project the store knows, including any whose checkout is not on this machine — the same set the dashboard shows.

### `story project link origin [URL]` / `story project unlink origin [URL]`

The git origins a project answers to. A checkout of a registered origin resolves to that project with no flag and no pointer file, which is what makes a fresh clone work immediately.

- One project may hold **many** origins; an origin belongs to **at most one** project. Registering one another project already holds is refused, naming that project.
- `URL` may be omitted, and is then the origin *this directory's own repository* records. storyhook reads exactly `git config --get remote.origin.url` — which **walks up the directory tree**, so the omitted form additionally requires you to be at the repository's top level. From a subdirectory it would otherwise register the enclosing repository's identity against this project, permanently, locking out every sibling project in that repository.
- The project is named the ordinary way: `--project <slug>`, `$STORYHOOK_PROJECT`, or a working directory that already resolves.

### `story project link checkout [PATH]` / `story project unlink checkout`

Where a project's repo-side work runs. **At most one per project**, and **never** consulted to decide which project you are in — linking a directory does not make commands run there resolve to that project. `PATH` defaults to the current directory; linking a second replaces the first and reports what it replaced.

> **Naming hazard.** `story link` / `story unlink` are aliases for `story relate` / `story unrelate` and join one **story** to another. `story project link` / `unlink` are about **git**. Same word, unrelated subjects.

### `story project delete [--force]`

**Permanently deletes** the project, every story, every event, every checkout registration and every registered origin. There is no undo.

- Always asks first; the confirmation is the project's slug typed in full
- `--force` skips the question, and is **required** under `--json` or with no terminal to ask at
- **No positional.** The project is named the ordinary way: `--project <slug>`, `$STORYHOOK_PROJECT`, or a working directory that already resolves — which is also how a project whose checkout is gone is reached
- **Touches no files.** The `.storyhook.toml` and `AGENTS.md` in every checkout are left where they are; the warning lists those directories so you know which ones now claim an identity that does not exist

### `story member add "<name <email>>"` / `story member add -g <github-handle>`

Register a team member for story assignment.

```bash
story member add "Alice <alice@example.com>"
story member add -g octocat
```

### `story state add <slug> --super OPEN|CLOSED [--role active]`

Add a custom workflow state. `todo`/`in-progress`/`blocked`/`done` already exist after `init` and cannot be removed — only use this for additional states (e.g., `review`, `verifying`, `wont-fix`).

- `--super OPEN` or `--super CLOSED` — whether the state counts as open or closed work
- `--role active` — marks this as *the* in-progress-equivalent state for automation

```bash
story state add review --super OPEN
story state add wont-fix --super CLOSED
```

### `story state remove <slug>`

Remove a custom workflow state. Fails if any story is currently in that state — move affected stories first.

```bash
story state remove review
```

### `story type list` / `story type add <slug> [--description "<text>"] [--emoji <glyph>]` / `story type set <slug> [...]` / `story type remove <slug>`

Manage story types. Defaults after `init`: `normal` 📙, `epic` 📚, `bug` 🐞, `chore` 🧺 — each with an emoji the web dashboard renders next to stories of that type. Set a story's type at creation with `story new <title> --type <slug>`, or with `story set <id> --type <slug>`. `story type set` edits an existing type's description and/or emoji in place (`--no-description`/`--no-emoji` clear rather than set); a type with no emoji renders a generic 🏷️ on the dashboard.

```bash
story type list
story type add spike --description "Time-boxed research task" --emoji 🔬
story type set spike --emoji 🔍
story type remove chore
```

### `story scaffold agents-md|claude-md|cursor-rules`

Generate agent instruction templates for integration with AI coding tools.

```bash
story scaffold claude-md
story scaffold agents-md
story scaffold cursor-rules
```

### `story plugin install|uninstall <target>`

Register or remove an editor/agent plugin integration. Currently the only supported target is `claude-code`.

```bash
story plugin install claude-code
```

---

## Creating & Importing Stories

### `story new <title> [--state <slug>] [--type <slug>]`

Create a new story with the given title. Returns the new story ID. Title should be quoted.

- New stories start in the first OPEN state (`todo`) unless `--state` is given
- **Use when**: adding a single new task, bug, or feature to track. For bulk creation from a spec, use `story decompose`.

```bash
story new "Implement user authentication"
story new "Fix crash on launch" --type bug
```

### `story decompose <file> [--dry-run]` / `story decompose --stdin [--dry-run]`

Parse a markdown or YAML spec into stories with relationships, priorities, and labels, in a single call.

- `### Wave N` headings add a `blocked-by` edge from every story in wave N+1 to every story in wave N (and a `phase:N` label)
- `- [ ]` items become stories; `[HIGH]`/`[LOW]`/etc. prefixes set priority; `#tag` becomes a label; heading nesting becomes `child-of`
- `--dry-run` previews the parsed stories (as JSON) without creating anything
- `--stdin` reads the spec from stdin instead of a file — use this to feed a `PLAN.md` directly

```bash
story decompose spec.md --dry-run          # preview
story decompose spec.md                    # create for real
cat PLAN.md | story decompose --stdin --json
```

### `story import [<file>]`

Import stories from a JSON file (or stdin) — an array of story objects with at minimum a `title` field. For markdown/YAML specs, use `story decompose` instead.

```bash
story import stories.json
cat stories.json | story import
```

### `story import-project <file> [--legacy-links]`

Restore a **full project snapshot** (states, types, members, and stories) previously produced by `story export`. This is whole-project migration/backup, not a single-story import — use `story import` for that.

`--legacy-links` asserts that `<file>` predates event kind #18 (`StoryCommitLinked`), so its `[git] <sha>: <subject>` comments are pre-existing commit-link records rather than prose a user typed — and projects them into the store's link table accordingly. Omit it (the default) for a document from a current binary; passing it against one mixes a real user comment shaped like a link into the link table if that comment is present.

```bash
story import-project backup.json
story import-project old-backup.json --legacy-links
```

### `story export`

Export the entire project (schema version, prefix, states, types, members, and every story with its full event history) as a single JSON document. Pairs with `story import-project` for backup/migration.

```bash
story export > backup.json
```

### `story migrate [<path>] [--dry-run]`

Move an existing `.storyhook/` project into storyhook's store. Reads the legacy tree — states, types, members, every story's event history, the archive, and the story-number counter — and writes it into the store as one project. The `.storyhook/` directory is **never modified**: it is the rollback, and it should stay in the repository until the result has been checked.

With no path argument the command walks up from the working directory to find the project, so it can be run from anywhere inside the repository.

```bash
story migrate --dry-run       # See the plan, including any repairs, and write nothing
story migrate                 # Do it
story migrate ../other-repo   # Migrate a project you are not standing in
```

Repairs and refusals, both reported per instance:

- A relation only one story's history recorded is **completed** — the missing half is written as an event carrying the original instant.
- A parentage only one story recorded, where the same child has a parent both stories recorded, is **retracted**: agreement outranks assertion. The original claim stays in the event log; only the read model changes.
- Anything that cannot be settled without guessing is **refused with nothing imported**: two parents that both agree, a relation pointing at a story that is not there, a story that exists both open and archived, or a story sitting in a state `states.toml` no longer defines.

It refuses to run in a linked git worktree (a worktree's `.storyhook/` is a diverged copy, and migrating it would create a second project with the same prefix), and refuses to run twice against one checkout. Event kinds written by a newer storyhook are carried over verbatim.

---

## Finding & Viewing Stories

### `story show <id>`

Show full details for a story: title, state, priority, labels, comments, relationships.

```bash
story show SH-3
story show SH-3 --json
```

### `story search <query>`

Full-text search across story titles, comments, and labels (open and archived).

```bash
story search "authentication"
```

### `story list [options]`

List open stories with optional filters (combine as many as needed).

| Filter | Description |
|---|---|
| `--state <slug>` | Filter by state (e.g., `todo`, `in-progress`, `done`) |
| `--assignee <id\|handle>` | Filter by assigned member |
| `--flagged` | Show only stories with integrity warnings |
| `--priority <levels>` | Filter by priority (comma-separated: `critical,high`) |
| `--label <labels>` | Filter by labels (comma-separated) |
| `--phase <N>` | Filter by phase number (the `phase:N` label convention) |
| `--created-after <date>` | Stories created after date (ISO 8601) |
| `--updated-after <date>` | Stories updated after date (ISO 8601) |
| `--blocked` | Show only stories blocked by a dependency or `block` |
| `--ready` | Show only ready stories (unblocked, all predecessors closed) |
| `--stale <duration>` | Stories not updated within duration (e.g., `3d`, `1w`) |

```bash
story list
story list --state in-progress
story list --blocked --json
story list --priority critical,high
story list --ready --json
```

### `story next [--count <n>] [--phase <N>]`

Get the highest-priority ready stor(y/ies) — ready meaning all predecessors (`blocked-by` relations) are closed and there's no active `block`.

- Default count is 1
- **Use when**: picking what to work on next
- **Note the JSON shape difference** — see "JSON Output Shapes" below: a single result nests under `.story.story`, multiple results nest under `.stories[].story`, and no ready stories returns `.message` instead of `.story`

```bash
story next
story next --json
story next --count 3 --json
```

### `story summary`

Compact project overview: counts by state/priority/type, blocked/flagged/ready counts, and the list of ready stories.

```bash
story summary
story summary --json
```

### `story report [--html]`

Generate a project report — markdown by default, or a standalone HTML document with `--html`.

```bash
story report
story report --html > report.html
```

### `story load-context [--format markdown|json]`

Comprehensive session-start context: project state, open stories, blocked items, ready tasks, phase progress. (Previously named `story context`; that alias still works.)

```bash
story load-context
story load-context --format json
```

### `story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]`

Analyze the dependency graph built from `blocks`/`blocked-by` relationships.

| Mode | Description |
|---|---|
| (no flag) | Overview: critical path, parallel groups, and root/leaf/edge counts |
| `--critical-path` | Only the longest dependency chain |
| `--blocked-by <id>` | Only what is transitively blocked by this specific story |
| `--parallel-groups` | Only the groups of stories with no dependency between them |

```bash
story graph
story graph --critical-path
story graph --blocked-by SH-1
story graph --parallel-groups --json
```

### `story session-start`

Emit `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"..."}}` for editor/agent session-start hooks (compact CLI reference + open/ready counts + next recommended story). The context is injected silently into the model's context, not shown to the user. Not normally invoked by hand.

```bash
story session-start
```

---

## Updating Stories — Single-Field Verbs

Each of these mutates exactly one aspect of a story and returns the updated `StoryView` (or plain text without `--json`).

### `story move <id> <state-slug> ["<comment>"]`

Transition a story to a new state, optionally logging a comment in the same call. Moving to a CLOSED-superstate state (e.g. `done`) auto-archives the story.

```bash
story move SH-3 in-progress
story move SH-3 done "All tests passing, feature complete"
```

### `story comment <id> "<text>"`

Append a timestamped comment. Comments are append-only and form the audit trail — use for progress notes, decisions, or context worth preserving.

```bash
story comment SH-3 "Implemented the database schema migration"
```

### `story assign <id> <member-id|handle>`

Assign a story to a team member (must already be registered via `story member add`).

```bash
story assign SH-3 alice
```

### `story prioritize <id> <critical|high|medium|low|none>`

Set priority. Affects `story next` ordering.

```bash
story prioritize SH-3 high
```

### `story label <id> <labels-csv>` / `story unlabel <id> <labels-csv>`

Add or remove one or more comma-separated labels. There is no `--remove` flag on `label` — removal is the dedicated `unlabel` command. Comma is always the label delimiter — a single label can never contain one, on this command or any other that accepts labels.

```bash
story label SH-3 backend,auth
story unlabel SH-3 auth
```

### `story block <id> "<reason>"` / `story unblock <id>`

Mark a story as blocked with a reason (excludes it from `story next` and `--ready`), or clear that status.

```bash
story block SH-3 "waiting on API spec from vendor"
story unblock SH-3
```

### `story reopen <id>`

Reopen a closed/archived story, returning it to an open state.

```bash
story reopen SH-3
```

### `story delete <id> "<reason>"`

Soft-delete with a required reason — archived, never truly lost; excluded from `story list` but findable via `story search`.

```bash
story delete SH-3 "duplicate of SH-1"
```

### `story purge <id> [--force]`

**Irreversible.** Removes a soft-deleted story and every trace of it — events, comments, labels, git links. Refuses a story that has not been soft-deleted first, so `story delete` is always the step before it.

Without a terminal (scripts, CI, agents) it refuses and names `--force`, because the confirmation is the story's id typed back. Any surviving story that still claims a relationship with the purged one has that claim retracted first, and the purged id is never reused.

```bash
story delete SH-3 "created in error"
story purge SH-3 --force
```

---

## Updating Stories — Multi-Field: `set`

### `story set <id> [--title "<title>"] [--state <slug>] [--priority <level>] [--assignee <member>] [--labels "<csv>"] [--blocked "<reason>"] [--unblocked] [--json '{...}'] [--type <slug>]`

Update several fields in one call — the preferred form for scripted/deterministic updates instead of chaining several single-field verbs. For a single field, the dedicated verb (`move`, `prioritize`, `assign`, `label`, `block`/`unblock`) is more concise and readable.

`--json` accepts an object with any of these keys: `title`, `state`, `priority`, `assignee`, `labels`, `blocked`, `story_type`. It is a way to set the same fields as the flags above in one payload — it is **not** a way to add a comment (use `story comment` for free text) and it does **not** accept arbitrary custom keys.

```bash
story set SH-1 --priority high --state in-progress
story set SH-1 --assignee alice --labels "backend,urgent"
story set SH-1 --json '{"state": "done", "priority": "high"}'
story set SH-1 --blocked "waiting for deploy"
story set SH-1 --unblocked
```

---

## Relationships

### `story relate <a> <relationship> <b>` / `story unrelate <a> <relationship> <b>`

Add or remove a relationship between two stories. (`link`/`unlink` are exact aliases for `relate`/`unrelate`.)

**These 8 relations are the entire vocabulary** — anything else errors with `unsupported relationship`:

| Relationship | Meaning | Inverse |
|---|---|---|
| `blocks` | A must be closed before B is ready | `blocked-by` |
| `blocked-by` | A is not ready until B is closed | `blocks` |
| `parent-of` | A is a parent/epic containing B | `child-of` |
| `child-of` | A is a sub-task of B | `parent-of` |
| `relates-to` | A is informationally related to B | `relates-to` (mutual) |
| `duplicate-of` | A is a duplicate of B | — |
| `obviates` | Completing A makes B unnecessary | `obviated-by` |
| `obviated-by` | B completing makes A unnecessary | `obviates` |

`blocks`/`blocked-by` are the **only** relations `story next`, `story list --ready`, and `story graph` treat as dependency edges — use them (not `relates-to`) to gate execution order. There is no `precedes`/`follows`/`starts-*`/`finishes-*`/`coincides-with`/`conflicts-with` — those do not exist in this CLI.

```bash
story relate SH-1 blocks SH-2          # SH-2 is not ready until SH-1 is done
story relate SH-3 parent-of SH-4
story relate SH-5 relates-to SH-6
story unrelate SH-1 blocks SH-2
```

---

## Hierarchy: Epics & Phases

### `story epic list` / `story epic show <id>` / `story epic create "<title>"` / `story epic add <epic-id> <story-id>`

Epics are stories of type `epic` that track child-story progress via `parent-of` relationships.

```bash
story epic create "Authentication overhaul"
story epic add SH-5 SH-1
story epic show SH-5
story epic list
```

### `story phase list` / `story phase show <N>` / `story phase add <id> <N>` / `story phase remove <id>` / `story phase create <N> ["<title>"]`

Phases are sugar over the label system — a story in phase 2 has the label `phase:2`. `story decompose` auto-assigns phases from `### Wave N` headings.

```bash
story phase list
story phase show 1
story phase add SH-5 2
story phase create 1 "Foundation"
```

---

## Dashboards: TUI & Web

### `story tui`

Launch an interactive terminal UI (dashboard + Kanban-style board).

```bash
story tui
```

### `story web start [--port <PORT>]` / `story web stop` / `story web status`

Live-updating local web dashboard (binds to `127.0.0.1`, default port `3456`).

```bash
story web start
story web stop
```

---

## Git & GitHub Sync

### `story commit-sync [--since <duration>]`

Scan recent git commits for story ID references and add comments. A commit that **claims** a story also moves it into the active state. (Previously `sync-git`; that alias still works.)

A claim is a claim word — `close`, `fix`, `resolve`, `implement`, `complete`, `start`, `wip`, in any tense — immediately before the id on the same line. `Closes SH-1` and the git trailer `Closes: SH-1` both claim; `Refs SH-1`, `see SH-1` and a bare `SH-1` only link. `fix: SH-1 broken parser` only links, because there the colon is a Conventional Commits type rather than a trailer key. The words `not`, `no`, `never`, `without`, `unless` or an `n't` word before the claim word cancel it, as does a `Revert "…"` subject.

A closed story still links — the common shape is a merge commit whose body closes the very story its own PR just closed — but it never moves; reopen it to move it.

Each run reports why a story that linked did not also move — no claim word, `sync.auto_transition` off, no active state configured, the story is closed, or it is already out of its default state. `story project settings set sync.auto_transition false` stops even a claim from moving a story.

```bash
story commit-sync --since 1d
```

### `story github-sync [<id>] [--dry-run] [--resolve local|remote] [--strategy <s>] [--mode <m>]`

Bidirectional sync with GitHub Issues (three-way merge). Requires `STORYHOOK_GITHUB_TOKEN`.

A field both sides changed to different values is a **conflict**, and storyhook does not decide it. Everything else in the sync still applies; the conflict is printed with all three values (base, local, remote) and the command **exits 8**. The merge base holds the disputed field, so re-running finds the same conflict rather than quietly taking GitHub's value. Answer it with `--resolve` on that one story — the flag requires an explicit `<id>`, because a whole-sync resolution would decide conflicts you have not read — or set the field to the same value on both sides and re-run.

**First sync on a project** is asked how to handle it — interactively, or non-interactively (a script, `--json`) told up front with `--strategy` and `--mode` together:

```bash
story github-sync --strategy future-only --mode manual
```

`--strategy` is `import-all` / `match-titles` / `push-only` / `future-only`. `--mode` is `manual` (the default) or `off`; `auto` is refused by name — storyhook implements no sync-on-every-change mode.

**Changing the mode later**, on a project that is already configured: `--mode` alone (no `--strategy`, no `<id>`) changes the stored mode instead of syncing — the way to turn a disabled (`off`) project back on, or to repair one still carrying `auto` from before the rearchitecture, which this build refuses to run under and reports rather than silently ignoring. Needs no GitHub token.

```bash
story github-sync --dry-run
story github-sync SH-1
story github-sync SH-1 --resolve remote
story github-sync --mode manual   # turn sync back on / repair a stuck mode
```

### `story link-pr <id> <url> [--no-close-on-merge]`

Link a GitHub pull request to a story, by its web URL (`https://github.com/<owner>/<repo>/pull/<number>`). Needs no GitHub token — the URL is parsed, not fetched — so this works whether or not a `github-sync` remote is configured, and in a build without the `github-sync` feature.

`close_on_merge` defaults to true: `story pr-check` closes the story once this pull request merges. If the project has a configured `github-sync` remote and the URL's `owner/repo` disagrees with it, a `close_on_merge: true` link is refused — pass `--no-close-on-merge` for a deliberate cross-repository bookmark, which nothing can auto-close.

Re-linking a URL a story already links updates its `close_on_merge` flag rather than erroring.

```bash
story link-pr SH-1 https://github.com/acme/widgets/pull/42
story link-pr SH-1 https://github.com/acme/widgets/pull/42 --no-close-on-merge
```

### `story unlink-pr <id> <url>`

Remove a previously-linked pull request from a story, by the same URL it was linked with. Needs no GitHub token, same as `link-pr`.

### `story pr-check [<id>]`

Check a story's linked pull requests against GitHub — one story, or every open story project-wide with no `<id>`. Requires `STORYHOOK_GITHUB_TOKEN` and a configured `github-sync` remote (`story github-sync` sets one up).

A merged pull request whose link has `close_on_merge: true` closes the story, in the same transaction as the merge is recorded. A pull request closed without merging is recorded but never closes anything. A link whose `owner/repo` no longer matches the project's *currently* configured remote is skipped, not acted on — the remote may have been repointed since the link was made.

```bash
story pr-check
story pr-check SH-1
```

### `story hooks install|uninstall|list|test <event_type>`

`install`/`uninstall`/`list` manage **git** hooks (`post-commit`, `post-merge`, `prepare-commit-msg`) that drive automatic story syncing. `test <event_type>` fires a test **event** hook from the `[hooks]` table of `.storyhook.toml`, falling back to a legacy `.storyhook/hooks.toml` — valid event types are `create`, `state_change`, `close`, `comment`, `priority_change`, `label_change`, `relationship_change` — and requires hooks to be configured already; it errors if none are.

```bash
story hooks install
story hooks list
story hooks test create
```

### `story handoff [--since <duration>]`

Generate a session handoff document summarizing recent activity (default: last 2 hours).

```bash
story handoff --since 4h
```

---

## Project Health

### `story doctor [--fix]`

Diagnose integrity issues: orphaned relationships, invalid states, data inconsistencies. `--fix` attempts automatic repair.

```bash
story doctor
story doctor --fix
```

---

## JSON Output Shapes

Every command accepts the global `--json` flag and, on success, wraps its payload in a common envelope: `{"result": "ok", ...}`. **Story data is double-nested** — this is the single most common mistake when parsing storyhook output, so read carefully before writing a `jq` selector.

- **Single-story commands** (`show`, `new`, `comment`, `assign`, `move`, `block`, `unblock`, `prioritize`, `label`, `unlabel`, `reopen`, `relate`, `set`, `link-pr`, `unlink-pr`) return the story under `.story.story` — e.g. `.story.story.state`, **not** `.story.state`.
- **List commands** (`list`, `search`, `import`, `decompose`) return an array under `.stories[]`, where each element is itself the same wrapper — e.g. `.stories[].story.state`, **not** `.stories[].state`.
- **`story next --json`** is context-dependent:
  - One ready story → `.story.story.state` (same single-story shape as above)
  - Multiple ready stories with `--count N` → `.stories[].story.state` (same list shape)
  - No ready stories → there is no `.story` key at all; check `.message` (`"no ready stories"`)
- **`story summary --json`** → `.summary.{total_open,total_closed,by_state,by_priority,blocked_count,flagged_count,ready_count,ready_stories}` (`ready_stories` follows the same list shape above)
- **`story graph --json`** → `.graph.{critical_path,parallel_groups,overview}` with no mode flag; passing `--critical-path`, `--blocked-by <id>`, or `--parallel-groups` populates only that one field
- **`story doctor --json`** → on a healthy project, `.findings[]` (always present, always empty) and `.advice[]` (array of strings). `.issues[]` is the deprecated spelling of `.advice[]` and carries the same list for one more release. On a **damaged** project doctor *fails* — exit 5, `.result == "error"` — and the report rides the error envelope: `.findings[]` with `.code`/`.subject`/`.remedy`/`.message`, plus `.data` for the checks that hold more than a sentence (a read-model divergence carries `.data.divergence.{field,persisted,rebuilt}`). `.error` is those findings' messages joined, then `.advice[]`
- **Message-only commands** (`init`, `member add`, `state add/remove`, `handoff`, `commit-sync`, `github-sync`, `pr-check`, `hooks *`, `scaffold`) → `.message` (a plain string) when the **global** `--json` flag is passed
- **`story export`** and **`load-context --format json`** / **`context --format json`** print their document as raw JSON — no envelope. The global `--json` and `--quiet` flags do not change them: `story export`, `story export --json` and `story export --quiet` all emit the same bytes (likewise for `context --format json`), and export's are accepted by `story import-project`

Errors (any command) look like:

```json
{
  "result": "error",
  "error": "story `SH-99` not found",
  "exit_code": 3
}
```

Exit codes: `0` success · `2` usage/validation error · `3` not found · `4` lock timeout (another process holds the project lock) · `5` integrity/storage error.

**Example — one ready story:**

```bash
$ story next --json
{
  "result": "ok",
  "story": {
    "story": { "id": "SH-2", "state": "todo", "priority": "high", ... }
  }
}
```

**Example — no ready stories:**

```bash
$ story next --json
{ "result": "ok", "message": "no ready stories" }
```

For the full generated reference straight from the binary (kept in sync with every release), run `story help --all` or `story help <command>` directly.
