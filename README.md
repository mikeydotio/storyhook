# story

`story` is a CLI-first story and issue tracker built for local repositories, scripting, and AI coding agents.

It keeps every project's stories in one local SQLite store as an append-only event log, and favors short commands that are easy to type, pipe, and automate.

## Why `story`

- Local-first: everything lives on your machine, in one store you own — no server, no account.
- Agent-friendly: concise commands, stable `--json` output, and explicit exit codes.
- Audit-friendly: a story *is* its event history; the row you read is a fold of it, and `story doctor` checks that it still is.
- Safe under concurrent use: writes are SQLite transactions, so two agents, a git hook and the dashboard can share one store.

## Current capabilities

- Create and show stories
- Add comments with `story <id> "comment"`
- Assign members
- Define project states mapped to `OPEN` or `CLOSED`, and edit, reorder, or
  remove them later from the CLI, the web dashboard, or the TUI
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
| `/story do <id> [--auto]` | Claims a **ready** story and dispatches it to a fresh plan-mode Claude session in a new tmux window rooted in a per-story git worktree |
| `/story complete <id>` | Closes the story and reclaims its worktree and merged branch, after showing you a plan and asking |
| `/story capture <id>` | Dumps the recent output of a dispatched session's window (read-only) |
| `/story doctor` | Checks project data integrity, and whether this Claude build's readiness and paste behavior is still recognised |

`do`, `capture`, and `doctor` need tmux. `do` refuses a story that isn't ready —
closed, blocked, awaiting, obviated, or already in progress — and names the
reason rather than dispatching it.

`--auto` keeps plan approval as the *only* human interaction: choose
auto-accept edits when you approve, and the session runs to completion on its
own — resolving open questions by `/council-vote`, running the full test
suite, merging its own PR with a merge commit, and closing the story if the
work is genuinely done (or blocking it and stopping on a hard stop, such as
red tests or a failed merge, that a vote can't resolve).

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

Create a project inside the repository you want to track. With no flags at a terminal it asks; with any flag it never does, and `--prefix` is required:

```bash
story project new --prefix SH
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
story project new --prefix <PREFIX> [--name <NAME>]
                  [--attach <PATH> | --no-attach] [--no-agents-md]
story project delete [--force]
story project list
story project settings list | get <key> | set <key> <value> | unset <key>
story new <title>
story member add "<name <email>>"
story member add -g <github-handle>
story state list
story state add <state-slug> --super OPEN|CLOSED [--role active]
                             [--description "<text>"]
story state set <state-slug> [--super OPEN|CLOSED] [--role active|none]
                             [--description "<text>"] [--no-description]
                             [--move-stories-to <state-slug>]
story state remove <state-slug> [--move-stories-to <state-slug>]
story state reorder <state-slug,state-slug,...>
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

### Story ids

Wherever an id is expected, both forms name the same story — the canonical
`SH-5`, and the bare number `5` on its own:

```bash
story show 5           # the same story as `story show SH-5`
story move 5 done
story relate 5 blocks SH-7   # mixed, and fine
```

The prefix lives only inside a canonical id. It is how an id is *rendered*, not
how a story is *addressed*: a bare number is read against whichever project the
command is acting on, so `5` means nothing until that is settled. Run one with
no project and you get the same refusal every other command gives — which names
the ways out — rather than a story that does not exist.

An id carrying a **different** project's prefix is refused rather than resolved.
`--project` decides which project you are in, and an id never overrides it:

```console
$ story --project storyhook show CAL-1
error: story id `CAL-1` does not belong to project `storyhook`.

  id     CAL-1 - prefix `CAL`, held by project `scad-caliper`
  scope  storyhook - prefix `SH`

Nothing has been read or written. Re-run it naming the story's own project:

  story --project scad-caliper <command>
```

## States

- Every project state maps to exactly one superstate: `OPEN` or `CLOSED`.
- Every project has `todo`, `in-progress` and `blocked` as `OPEN` states and
  `done` as a `CLOSED` one. Those four cannot be removed and their superstates
  cannot be changed; anything else you add is yours to arrange. A project
  created before this rule reports it in `story doctor`, and
  `story doctor --fix` adds whatever is missing.
- A project must have at least one `OPEN` state and at least one `CLOSED` state.
- Moving a story into a `CLOSED` state immediately archives it to SQLite.
- Closed stories remain readable but are no longer mutable.
- State order is user-visible: it's the dashboard board's column order, and
  new stories land in the first `OPEN` state. `story state reorder` sets it.
- Slugs are lowercase letters, digits, and single dashes (`in-review`) —
  they're typed as CLI arguments and appear in dashboard URLs.
- At most one state may carry `--role active`, marking the state a story
  enters when work starts — where `story commit-sync` moves a story that a
  commit *claims* (`Closes SH-1`), as opposed to merely names (`Refs SH-1`).
- There is no rename: a slug is recorded in every state-change event ever
  written. Add the new state, migrate to it, and remove the old one — which is
  therefore not a way around the four states every project must have.

Configure states from the CLI (`story state …`), the dashboard
(**Settings → Statuses**), or the TUI (press `s`) — all three go through the
same operations.

### Editing a state that still holds stories

Changing a state's superstate reclassifies every story sitting in it, and
removing a state would leave those stories pointing at a definition that no
longer exists. Both refuse until you say where the stories should go:

```bash
story state remove review --move-stories-to todo
story state set review --super CLOSED --move-stories-to todo
```

Stories moved into a `CLOSED` state are closed and archived exactly as
`story move` would, and each records a comment noting the move. A state that
already has **archived** stories can't be removed at all: their history
records the slug, and reopening one later would fail against a state that no
longer exists.

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

Stories live in one store outside your repositories:

```text
$XDG_DATA_HOME/storyhook/store.db      # ~/.local/share/storyhook/store.db
$XDG_STATE_HOME/storyhook/             # ~/.local/state/storyhook/
  daemon.json  daemon.pid  daemon.log
  backups/
```

What a repository carries is one committed file:

```toml
# .storyhook.toml
schema = 1
uuid = "291ea25f-3363-4b5d-9051-66636c1066f9"
prefix = "SH"

[plugin]          # optional, user-authored; storyhook reads it and never writes it
enabled = true
tracking = "normal"
```

Behavior:

- Every story is an append-only event history; the queryable row is a fold of it.
- Commands resolve their project by, in order: `--project <slug>`,
  `$STORYHOOK_PROJECT`, the nearest committed `.storyhook.toml` at or above the
  working directory (never climbing past the repository's own top level), and
  the repository's registered git origin. Nothing about the filesystem is ever
  *required*, so a fresh clone at a path storyhook has never seen still finds
  its stories — and a directory that names no project refuses rather than
  guessing.
- **Every checkout of a repository is the same project.** Linked worktrees
  included: there is no per-checkout copy to diverge.
- The story ID counter is per project and monotonic: `SH-1`, `SH-2`, `SH-3`, ...
- Writes are transactions. A failed write leaves nothing behind, including the
  story number it would have used.
- Migrating from the old per-repository layout: `story migrate`. It never writes
  to the `.storyhook/` directory it reads — that directory is your rollback.

## Web dashboard

`story` includes a local web dashboard for browsing and triaging stories visually, alongside the CLI. One dashboard serves **every project the store knows** — a home screen with a summary card per project, a header project selector for fast switching, and, per project, the same Board/List/drawer views a single-project dashboard has always had.

There is no registration step. A project reaches the dashboard by existing: `story project new` puts it in the store, and the store is what the dashboard reads. `story project list` prints the same set.

```bash
story web start [--port <PORT>]      # start the dashboard (default port 3456)
story web stop                       # stop it
story web status                     # check whether it's running
```

`story web start` launches the dashboard on its own — it does not require running from inside a project. There is nothing to register: every project already in the store shows up immediately, and a project created afterward (from any client) appears without restarting the dashboard.

Open the URL printed on start — `http://127.0.0.1:<port>` by default, or your machine's Tailscale MagicDNS name (falling back to its tailnet IP) if Tailscale is running (see Network exposure below). The dashboard offers:

- **Home** — a summary card per project (open/ready/blocked counts). Click a card to open that project. A project whose data can't currently be loaded (its checkout moved, or some other read failure) shows its error instead of a summary, rather than failing the whole page.
- **Settings** — create a new project, or delete an existing one. Deleting removes the project and everything recorded against it from the store; it never touches the project's own files on disk. **Statuses** on any project row opens that project's state configuration: reorder (which is the board's column order), flip open/closed, set the active role and descriptions, add and remove. Reclassifying or removing a status that still holds stories asks where those stories go first; deletion is disabled, with the reason, when a status has archived history or is the last open or closed one.
- **Board** — a kanban view with one column per project state, in `states.toml` order. Drag a card to a different column to move the story; dropping onto a `CLOSED` state archives it in place, and it stays visible in that column rather than vanishing.
- **List** — a filterable, sortable table view.
- **Detail drawer** — click any card or row to view and edit a story's full detail: title, state, priority, assignee, type, labels, block/unblock, comments, and relationships, plus reopen and delete.
- Faceted filters (priority, assignee, type, state) and free-text search, shared between both project views.
- Live updates over a server-sent-events stream — every write, from any client, appears without a reload — with a slow poll as a fallback for the rare case a push is missed. Dark mode follows your system theme.

It's a single self-contained page with no external dependencies (no CDN, no build step) and no mocked data — every action goes through the same validated, event-sourced write path as the CLI.

The dashboard is a single background daemon shared by every project — not one per project, and the same daemon the CLI talks to. The project list it shows is the store's own projects table, so there is no separate registry file to fall out of step with it. The daemon's portfile, PID file and log live under `$XDG_STATE_HOME/storyhook/` (`~/.local/state/storyhook/` by default).

> **Upgrading from a per-repo dashboard:** earlier versions ran one daemon per project, started from inside it, with its PID/lock/log under that project's own `.storyhook/`. If you have one of those still running, `story web stop` from this version won't see it — stop it manually, then start the new dashboard — it already knows every project in the store.

### Network exposure

The dashboard is reachable from **localhost and your tailnet only — never the public internet, never a plain LAN address**:

- It always binds `127.0.0.1`. This is hardcoded and not configurable.
- If the `tailscale` CLI is installed and reports an IP, it *also* binds that tailnet IP, so other devices on your tailnet can reach it directly — no reverse proxy needed. This is best-effort: if the bind fails for any reason, the dashboard keeps serving on localhost and logs a warning.
- It never binds `0.0.0.0` or any other wildcard/public-facing address, and it never binds a generic LAN IP.

If the `web-serve` tool is present on your `PATH` (coderig/agentsmith environments), `story web start`/`stop` additionally register/unregister the port with it — that tool's own access controls govern any exposure beyond what's described above.

### Security

Mutating requests (creating or deleting a project; creating, moving, editing, or deleting a story) require:

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

### Driving `story` from a test suite

If your suite shells out to `story` — to build a fixture project, exercise a
plugin, or drive an integration test — give it a store of its own. Story data
lives in one store shared by everything on the machine (see
[Storage model](#storage-model)); a suite that never names a store writes into
that same shared store, permanently, with no error and nothing to notice. One
run of one unisolated suite once put 394 junk projects into a real tracker this
way.

```bash
story store new /path/to/scratch/store.db   # create it once
story --store-path /path/to/scratch/store.db project new --prefix TST
```

`--store-path` (or its variable, `$STORYHOOK_STORE_PATH`) names a store on
every command and is the supported way to isolate a suite. `$STORYHOOK_DATA_DIR`
also works, but the flag and the variable above it both outrank it silently —
set at most one. Add these two if the suite runs more than one `story` command
so a spawned daemon behaves:

```bash
export STORYHOOK_DAEMON_ADDR=127.0.0.1:0   # a kernel-assigned port, not 3456
export STORYHOOK_PARENT_PID=$$             # the daemon dies with this run
```

`story` also refuses, on its own, to create a project at a path under a
temporary directory when the store it would write to is not itself temporary —
a backstop for a suite that built a fixture but forgot to name a store at all.
It is not a substitute for isolating deliberately: it only catches the shape
above, not every way a command can reach the wrong store.

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
