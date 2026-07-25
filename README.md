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

### Claude Code plugin

storyhook ships a Claude Code plugin (`plugin/claude-code/`) that adds a `/story` router
skill (`/story do`, `/story work`, `/story context`, …) and session hooks. There
are two ways to install it; both register it properly so Claude Code loads the
`/story` and `/story-*` commands.

**CLI-first** — if you already installed the `story` CLI (above):

```bash
story plugin install claude-code
```

This registers `mikeydotio/storyhook` as a marketplace and installs the plugin through the
`claude` CLI. It requires the `claude` CLI on your PATH.

**Marketplace-first** — if you prefer to install the plugin before the CLI:

```text
/plugin marketplace add mikeydotio/storyhook
/plugin install story@storyhook
```

The marketplace route installs the plugin but not the `story` CLI. Once the plugin loads,
run `/story-install` and the plugin will install and verify the CLI for you.

> After installing the plugin (either route), start a new Claude Code session so the
> `/story` and `/story-*` commands load.

#### The `/story` lifecycle verbs

`/story` covers a story end to end. The verbs below do deterministic work in
`plugin/claude-code/bin/story.sh`; the rest delegate to the individual
`/story-*` skills unchanged.

| Command | What it does |
|---|---|
| `/story` | Lists ready stories and asks which to pick up |
| `/story <id>` | Shows the story, then offers to start work on it |
| `/story new <description>` | Interrogates you, drafts the story, files it after you confirm |
| `/story view <id>` | Prints the story and its comments, then stops |
| `/story do <id>` | Claims a **ready** story and dispatches it to a fresh plan-mode Claude session in a new tmux window rooted in a per-story git worktree |
| `/story complete <id>` | Closes the story and reclaims its worktree and merged branch, after showing you a plan and asking |
| `/story capture <id>` | Dumps the recent output of a dispatched session's window (read-only) |
| `/story doctor` | Checks project data integrity, and whether this Claude build's readiness and paste behavior is still recognised |

`do`, `capture`, and `doctor` need tmux. `do` refuses a story that isn't ready —
closed, blocked, awaiting, obviated, or already in progress — and names the
reason rather than dispatching it.

`complete` is conservative by design. It never deletes an unmerged branch, never
removes a dirty, locked, or current worktree, and never touches `main` or the
default branch; anything preserved is reported and explained. Merged-ness is
judged against `origin/<default>` (freshened first) as well as your local
branch, so a repo whose local `main` lags still cleans up correctly.

## Uninstall

If installed via the install script or manual download:

```bash
rm ~/.local/bin/story
```

If installed via Cargo:

```bash
cargo uninstall storyhook
```

To remove the Claude Code plugin:

```bash
story plugin uninstall claude-code
```

This unregisters the plugin via the `claude` CLI, cleans up project-local config, and
removes any legacy plugin directory left by older versions.

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
story web start [--port <PORT>]
story web stop
story web status
story web register [<PATH>] [--name <NAME>]
story web deregister <ID|PATH>
story web list
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

## Web dashboard

`story` includes a local web dashboard for browsing and triaging stories visually, alongside the CLI. One dashboard serves every project you register with it — a home screen with a summary card per repo, a repo-select dropdown for fast switching, and, per repo, the same Board/List/drawer views a single-project dashboard has always had.

```bash
story web register                   # register the current directory
story web register ../other-project  # register another repo by path
story web register . --name "API"    # register with a display name
story web list                       # list every registered repo + id
story web deregister api             # remove one (by id or by path)

story web start [--port <PORT>]      # start the dashboard (default port 3456)
story web stop                       # stop it
story web status                     # check whether it's running
```

`story web start` launches the dashboard on its own — it does not require running from inside a project, and it does not auto-register anything. Repos only appear once you `register` them.

Open the URL printed on start — `http://127.0.0.1:<port>` by default, or your machine's Tailscale MagicDNS name (falling back to its tailnet IP) if Tailscale is running (see Network exposure below). The dashboard offers:

- **Home** — a summary card per registered repo (open/ready/blocked counts). Click a card to open that repo. A repo whose data can't currently be loaded (moved, deleted) shows its error instead of a summary, rather than failing the whole page.
- **Settings** — register a new repo, or deregister an existing one. Deregistering only edits the dashboard's registry; it never touches the repo's own files.
- **Board** — a kanban view with one column per project state, in `states.toml` order. Drag a card to a different column to move the story; dropping onto a `CLOSED` state archives it in place, and it stays visible in that column rather than vanishing.
- **List** — a filterable, sortable table view.
- **Detail drawer** — click any card or row to view and edit a story's full detail: title, state, priority, assignee, type, labels, block/unblock, comments, and relationships, plus reopen and delete.
- Faceted filters (priority, assignee, type, state) and free-text search, shared between both project views.
- Live updates via 3-second polling; dark mode follows your system theme.

It's a single self-contained page with no external dependencies (no CDN, no build step) and no mocked data — every action goes through the same validated, event-sourced write path as the CLI.

The dashboard is a single background daemon shared by every registered repo — not one per project. Registered repos live in `~/.storyhook/registry.toml`; the daemon's PID file, lock file, and log are at `~/.storyhook/web.pid` / `~/.storyhook/web.lock` / `~/.storyhook/web.log`.

> **Upgrading from a per-repo dashboard:** earlier versions ran one daemon per project, started from inside it, with its PID/lock/log under that project's own `.storyhook/`. If you have one of those still running, `story web stop` from this version won't see it (it only knows about the new global daemon) — stop it manually, then `story web register` your project(s) and start the new dashboard.

### Network exposure

The dashboard is reachable from **localhost and your tailnet only — never the public internet, never a plain LAN address**:

- It always binds `127.0.0.1`. This is hardcoded and not configurable.
- If the `tailscale` CLI is installed and reports an IP, it *also* binds that tailnet IP, so other devices on your tailnet can reach it directly — no reverse proxy needed. This is best-effort: if the bind fails for any reason, the dashboard keeps serving on localhost and logs a warning.
- It never binds `0.0.0.0` or any other wildcard/public-facing address, and it never binds a generic LAN IP.

If the `web-serve` tool is present on your `PATH` (coderig/agentsmith environments), `story web start`/`stop` additionally register/unregister the port with it — that tool's own access controls govern any exposure beyond what's described above.

### Security

Mutating requests (registering/deregistering a repo; creating, moving, editing, or deleting a story) require:

- a same-origin request — the dashboard's own page sets a custom `X-Storyhook` header that a cross-site request cannot replicate without triggering a CORS preflight the server never answers;
- a `Host` header that resolves to `127.0.0.1`, `localhost`, `::1`, the tailnet IP this instance bound itself, or — when Tailscale MagicDNS is on — this machine's full MagicDNS name (e.g. `host.tailXXXXX.ts.net`) — this is what stops DNS-rebinding, which the header check alone can't catch. The bare short hostname (`host`, without the `.ts.net` suffix) is deliberately *not* trusted: unlike the full name, it can resolve through a DNS search domain that isn't your tailnet's, so trusting it could reopen the exact rebinding this check exists to stop.

Read-only requests (`GET /`, `GET /api/repos`, `GET /api/repos/<id>/data`, `GET /api/repos/<id>/story/<sid>`) have no such restriction — they're reachable (but not writable) from anywhere the socket itself is reachable, i.e. localhost and your tailnet.

If you reverse-proxy the dashboard under a different hostname (e.g. via `web-serve`) and want writes to work there too, set `STORYHOOK_WEB_TRUSTED_HOSTS` to a comma-separated allowlist before starting the server:

```bash
STORYHOOK_WEB_TRUSTED_HOSTS=my-tailnet-host story web start
```

This only widens the `Host` allowlist for writes — it does not change what the server binds. Only set it to hostnames that are themselves no more exposed than your tailnet.

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
