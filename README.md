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
- Add comments with `story comment <id> "comment"`
- Assign members
- Define project states mapped to `OPEN` or `CLOSED`, and edit, reorder, or
  remove them later from the CLI, the web dashboard, or the TUI
- Set and clear `awaiting` blockers
- Classify every story by a configured type; omission uses the first (`normal`
  in the stock catalog)
- Set priority levels (critical, high, medium, low); new stories default to low
- Add and filter by labels/tags
- Search stories by title, comments, and labels
- Project summary with state/priority breakdown
- Find the next executable stories in dependency order (`story next`)
- Add and remove story relationships
- Derive read-only `ancestor-of` and `descendent-of` family relationships on show output
- Archive stories immediately when they move into a `CLOSED` state
- Reopen archived stories
- Bulk-create stories from a JSON array (`story import`), or back up and restore a whole project as one JSON document (`story export` / `story import-project`)
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

### Agent plugin

storyhook ships one canonical plugin payload (`plugins/story/`) for Claude Code and
Codex. Its ten skills cover story context, work, planning, setup, triage, handoff,
sync, installation, updates, and the full lifecycle router. The skills resolve their
bundled helper from the installed `SKILL.md` path, so they work from a marketplace
cache rather than depending on the source checkout or current working directory.

#### Claude Code

Claude exposes the skills as `/story` and `/story-*` commands and loads the provider's
session hooks. There are two ways to install it.

**CLI-first** — if you already installed the `story` CLI (above):

```bash
story plugin install claude
```

This registers `mikeydotio/storyhook` as a marketplace and installs the plugin through the
`claude` CLI. It requires the `claude` CLI on your PATH.
The former target `claude-code` remains accepted by `story plugin install` and
`story plugin uninstall` as a deprecated, warned compatibility alias.

**Marketplace-first** — if you prefer to install the plugin before the CLI:

```text
/plugin marketplace add mikeydotio/storyhook
/plugin install story@storyhook
```

The marketplace route installs the plugin but not the `story` CLI. Once the plugin loads,
run `/story-install` and the plugin will install and verify the CLI for you.

> After installing the plugin (either route), start a new Claude Code session so the
> `/story` and `/story-*` commands load.

#### Codex local marketplace

If the `story` CLI is already installed, use its supported installer:

```bash
story plugin install codex
```

It detects the current checkout during development and otherwise registers
`mikeydotio/storyhook`. It also installs an unversioned launcher at
`~/.codex/storyhook/story.sh` and a dedicated rule at
`~/.codex/rules/storyhook.rules`. The rule allows only `bash` followed by that exact
launcher path; it never allowlists bare `bash`. The launcher resolves Codex's currently
enabled Storyhook plugin version on every call, so cachebuster/version changes do not stale
the rule.

The installer verifies the rule with `codex execpolicy check`. Restart Codex afterward:
rules are loaded at startup. You can inspect the exact decision yourself:

```bash
codex execpolicy check --pretty \
  --rules ~/.codex/rules/storyhook.rules \
  -- bash ~/.codex/storyhook/story.sh context
```

See the [official Codex rules documentation](https://learn.chatgpt.com/docs/agent-configuration/rules)
for rule precedence and additional policy files.

For low-level development diagnosis, the marketplace half can be reproduced manually by
registering this checkout and adding the plugin through Codex:

```bash
codex plugin marketplace add /absolute/path/to/storyhook
codex plugin add story@storyhook
```

Those low-level Codex commands do not install the Storyhook launcher or rule; finish with
`story plugin install codex` before invoking helper-backed skills.

Start a fresh Codex conversation after installing. Invoke a skill explicitly with the
host's skill selector or describe the Storyhook workflow in natural language; activation
does not depend on Claude slash syntax. The Codex manifest deliberately declares only its
skills; current Codex discovers the shared `hooks/hooks.json` by convention. A local,
non-managed plugin may ask you to review/trust those SessionStart, PostToolUse(Bash), and
Stop hooks before they run.

#### Lifecycle router verbs

The `story` skill covers a story end to end. In Claude, these are `/story` commands;
in Codex, provide the same verb with the selected skill or state the intent naturally.
The verbs below do deterministic work in
`plugins/story/bin/story.sh`; the rest delegate to the individual
skills unchanged.

| Command | What it does |
|---|---|
| `/story` | Lists ready stories and asks which to pick up |
| `/story <id>` | Shows the story, then offers to start work on it |
| `/story new <description>` | Interrogates you, drafts the story, files it after you confirm |
| `/story view <id>` | Prints the story and its comments, then stops |
| `/story do <id> [--auto] [--force] [--agent=claude\|codex]` | Claims a **ready** story and dispatches it to a fresh provider Plan-mode session in a new tmux window rooted in a per-story git worktree; `--force` reuses an existing `in-progress` claim without weakening worktree/tmux safety |
| `/story complete <id>` | Closes the story and reclaims its worktree and merged branch, after showing you a plan and asking |
| `/story capture <id>` | Dumps the recent output of a dispatched session's window (read-only) |
| `/story doctor` | Checks project data integrity and the selected provider's readiness, Plan-mode, and paste behavior |

`do`, `capture`, and `doctor` need tmux. `do` refuses a story that isn't ready —
closed, blocked, awaiting, obviated, or already in progress — and names the
reason rather than dispatching it.

The dispatch adapter selects `STORY_AGENT=claude|codex`. An explicit `--agent` overrides
the adapter's host default, so `story.sh dispatch SH-123 --agent=codex` can launch Codex
from either host. The legacy `STORY_AGENT=claude-code` value remains a warned
compatibility alias. Claude keeps its existing
`.claude/worktrees/` and launch contract. Codex uses `.codex/worktrees/`, launches
`codex --no-alt-screen`, confirms the interactive screen, enters Plan mode with
Shift+Tab, and submits the bracketed-pasted charter with Tab. A failed readiness or
Plan-mode check rolls back the claim and worktree before any charter is submitted.

`--auto` is fully unattended while retaining Plan mode. Storyhook allows Claude's
plan exit through its packaged hook, then uses Claude's exact `PermissionRequest`
boundary to send Return to the selected Auto option in that session's tmux pane.
Claude launches with `acceptEdits` as the post-plan default. Codex starts an
exact-pane watcher after Plan mode is confirmed; it sends Return to the selected
“Yes, implement this plan” option, then workspace-write automatic review handles
later tool approvals. Codex also trusts the packaged hook for the invocation.
The hook refuses question tools instead of
waiting for a person who is not there. Claude can probe for `/council-vote`; Codex has
no stable machine-readable skill inventory, so its default is the safe solo charter
unless `STORY_COUNCIL=on` opts in explicitly. Both paths research and decide clear
questions, run tests, merge their PRs, close the story when its acceptance criteria
pass, and stop safely on a hard failure. A custom `STORY_LAUNCH_CMD` remains wholesale
and is reported as potentially weakening this unattended posture.
Because Codex changes modes through the UI rather than an `ExitPlanMode` tool call, the
built-in Codex prompt makes posting the exact approved plan to the story the first
implementation step. Wholesale custom prompt overrides must supply that instruction when
they need the same persistence guarantee.

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
story plugin uninstall claude
```

This unregisters the plugin via the `claude` CLI, cleans up project-local config, and
removes any legacy plugin directory left by older versions.

To remove a Codex marketplace installation created from a local checkout:

```bash
story plugin uninstall codex
```

This removes `story@storyhook`, the `storyhook` marketplace, and Storyhook's managed stable
launcher and rule. Unrelated or user-authored files are preserved.

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
story assign SH-1 mikey
story comment SH-1 "Parser skeleton is in place"
story block SH-1 "waiting on command grammar decision"
story unblock SH-1
story move SH-1 in-progress "Hooked up argument routing"
story move SH-1 done "Merged and verified"
```

Relate stories:

```bash
story relate SH-1 parent-of SH-2
story relate SH-2 blocks SH-3
story relate SH-4 relates-to SH-5
story unrelate SH-2 parent-of SH-3
```

Prioritize, label, and triage:

```bash
story prioritize SH-1 high
story label SH-1 backend,api
story next
story summary
story context
```

Inspect and report:

```bash
story show SH-1
story list --state todo
story list --assignee mikey
story list --flagged
story doctor
```

## Command reference

Every command below is real — checked by `tests/readme_command_reference.rs` against the
CLI's own parser, not against this page. If it's dispatchable, it's listed here; if it's
listed here, it parses.

```text
story --help
story --version
story help [<topic>] [--all] [--compact]
story update [--check] [--force]

story project new --prefix <PREFIX> [--name <NAME>] [--attach <PATH> | --no-attach] [--no-agents-md]
story project show
story project list
story project delete [--force]
story project set-prefix <NEW-PREFIX> [--force]
story project link origin [<url>]
story project link checkout [<path>]
story project unlink origin [<url>]
story project unlink checkout
story project settings list
story project settings get <key>
story project settings set <key> <value>
story project settings unset <key>

story new <title> [--state <slug>] [--type <slug>] [--description "<text>"] [--priority <level>] [--assignee <member>] [--label <name>] [--labels <csv>] [--draft]
story show <id>
story log <id>
story comment <id> "<text>"
story assign <id> <member>
story move <id> <slug> [--if-state <expected>] [--reason <text>] ["<comment>"]
story block <id> "<reason>"
story unblock <id>
story prioritize <id> <level>
story label <id> <labels-csv>
story unlabel <id> <labels-csv>
story close <id> "<reason>"
story reopen <id>
story archive <id>
story unarchive <id>
story archive-state <slug> [--force]
story publish <id>
story delete <id> [--force]
story set <id> (--title "<title>" | --state <slug> | --priority <level> | --assignee <member> | --labels "<csv>" | --blocked "<reason>" | --unblocked | --json "<json>" | --type <slug> | --description "<text>")  # at least one; combine as many as you like
story relate <a> <relationship-type> <b>
story unrelate <a> <relationship-type> <b>
story link <a> <relationship-type> <b>        # alias of relate
story unlink <a> <relationship-type> <b>      # alias of unrelate

story member add "<name <email>>"
story member add -g <github-handle>

story state list
story state add <slug> --super OPEN|CLOSED [--role active] [--description "<text>"]
story state set <slug> [--super OPEN|CLOSED] [--role active|none] [--description "<text>"] [--no-description] [--move-stories-to <slug>]
story state remove <slug> [--move-stories-to <slug>]
story state reorder <slug,slug,...>

story type list
story type add <slug> [--description "<text>"] [--emoji <glyph>]
story type set <slug> [--description "<text>"] [--no-description] [--emoji <glyph>] [--no-emoji]
story type remove <slug>

story phase list
story phase show <N>
story phase add <id> <N>
story phase remove <id>
story phase create <N> ["<title>"]

story epic list
story epic show <id>
story epic create "<title>"
story epic add <epic-id> <story-id>

story list [--state <slug>] [--assignee <member>] [--flagged] [--priority <levels>] [--label <labels>] [--created-after <date>] [--updated-after <date>] [--blocked] [--ready] [--stale <duration>] [--phase <N>] [--type <slug>] [--drafts] [--unassessed] [--include-closed] [--include-archived] [--all]
story next [--count <n>] [--phase <N>] [--epic <id>] [--exclude-label <csv>]
story claim <id> [--comment <text> | --no-comment] [--dry-run]
story claim --next [--phase <N>] [--epic <id>] [--exclude-label <csv>] [--comment <text> | --no-comment] [--dry-run]
story unclaim <id> [--comment <text> | --no-comment] [--dry-run]
story summary
story report [--html]
story search <query>
story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]
story context [--format markdown|json]
story load-context [--format markdown|json]   # alias of context
story handoff [--since <duration>]

story export
story import [<file>]
story import-project <file>
story decompose <file> [--dry-run]
story decompose --stdin [--dry-run]
story migrate [<path>] [--dry-run]

story doctor [--fix]
story doctor abandoned
story doctor abandoned clear (--all | <request-id>)
story doctor crashes
story doctor crashes clear (--all | <crash-id>)
story hooks install
story hooks uninstall
story hooks list
story hooks test <event_type>
story scaffold agents-md|claude-md|cursor-rules
story commit-sync [--since <duration>]
story sync-git [--since <duration>]           # alias of commit-sync
story link-pr <id> <url> [--no-close-on-merge]
story unlink-pr <id> <url>
story attachment add <id> <path> [--name <text>]
story attachment list <id>
story attachment remove <id> <n>
story attachment save <id> <n> <path>
story pr-check [<id>]
story github-auth login|status|logout
story plugin install <target>
story plugin uninstall <target>

story web start [--port <PORT>]
story web stop
story web status
story web open
story web address
story token new <name>
story token list
story token revoke <name>
story daemon start [--port <PORT>]
story daemon stop [--force]
story daemon status
story daemon install [--this-binary]
story daemon uninstall
story daemon token
story store new <path>
story store backup [--label <text>]
story tui
story mcp
story session-start
```

Global flags — `--json`, `--quiet`, `--no-hooks`, `--store-path <file>`, `--project <slug>`,
`--deadline <secs>` — precede the verb and work on any command; see
[Automation and scripting](#automation-and-scripting).

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

Direct relationship inputs `story relate`/`story unrelate` accept — this is the whole set:

- `blocks` / `blocked-by` — the only dependency edge; a chain of these is what `story graph`
  walks for critical-path and blocked-by analysis
- `parent-of` / `child-of` — hierarchy; see the epic scheduling notes below
- `duplicate-of` — no direction, same edge either way
- `relates-to` / `related-to` — a plain, undirected link with no scheduling meaning
- `obviates` / `obviated-by`

Derived, read-only relationships computed from the above and shown on story views — not
valid input to `relate`:

- `ancestor-of`
- `descendent-of`

Notes:

- Directional relationships automatically create their inverse on the related story.
- Mutual relationships create matching links on both stories.
- A story with children is a structural epic. Its state is computed recursively from its
  children, it carries no actionable steps of its own, and it does not appear in `story next`.
- An epic's priority remains stored independently. Among equal-priority ready stories,
  `story next` uses the most urgent direct parent epic as its first tie-breaker; a story may
  belong to several epics.
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
```

Behavior:

- Every story is an append-only event history; the queryable row is a fold of it.
- Commands resolve their project by, in order: `--project <slug>`,
  `$STORYHOOK_PROJECT`, the nearest committed `.storyhook.toml` at or above the
  working directory (never climbing past the repository's own top level), and
  the repository's registered git origin. Nothing about the filesystem is ever
  *required*, so a fresh clone at a path storyhook has never seen still finds
  its stories — and a directory that names no project refuses rather than
  guessing. `story project link checkout <PATH>` writes `.storyhook.toml` for
  a directory that has none, so it resolves from then on; it never overwrites
  one that already names a different project.
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

Open the URL printed on start — `http://127.0.0.1:<port>` by default. If Tailscale is running, the dashboard also binds your machine's tailnet IP (MagicDNS name, when available) in the background, moments after start; `story web start`'s own printed URL is always the loopback one, with a note when the tailnet address is still resolving — `story daemon status`/`address` show it once bound (see Network exposure below). The dashboard offers:

- **Home** — a summary card per project (open/ready/blocked counts). Click a card to open that project. A project whose data can't currently be loaded (its checkout moved, or some other read failure) shows its error instead of a summary, rather than failing the whole page.
- **Settings** — create a new project, or delete an existing one. Deleting removes the project and everything recorded against it from the store; it never touches the project's own files on disk. **Statuses** on any project row opens that project's state configuration: reorder (which is the board's column order), flip open/closed, set the active role and descriptions, add and remove. Reclassifying or removing a status that still holds stories asks where those stories go first; deletion is disabled, with the reason, when a status has archived history or is the last open or closed one.
- **Board** — a kanban view with one column per project state, in `states.toml` order. Drag a card to a different column to move the story; dropping onto a `CLOSED` state archives it in place, and it stays visible in that column rather than vanishing. A "Columns" filter-bar control picks which columns are shown, and a "Hide empty columns" toggle collapses any column with no currently-visible cards.
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
- If the `tailscale` CLI is installed and reports an IP, it *also* binds that tailnet IP, so other devices on your tailnet can reach it directly — no reverse proxy needed. This is best-effort: if the bind fails for any reason, the dashboard keeps serving on localhost and logs a warning. The bind itself happens on a background thread, after the dashboard is already serving loopback — a wedged or slow `tailscale` CLI delays only the tailnet interface's own availability, never the dashboard's.
- It never binds `0.0.0.0` or any other wildcard/public-facing address, and it never binds a generic LAN IP — enforced, not merely never attempted: the daemon refuses to serve a socket bound anywhere else.
- Every connection is checked again as it arrives, against the interface it arrived on: the loopback listener admits only a loopback peer, and the tailnet listener admits loopback plus Tailscale's own address ranges. A peer outside those ranges is refused before a single byte of its request is read — no `tailscale` process is ever consulted to decide this, so a wedged or missing `tailscale` CLI cannot affect it either way.

**Residual**: a subnet router, an exit node, or `iptables` forwarding can put genuinely off-tailnet traffic inside the ranges above — Tailscale ACLs, not this daemon, are the real membership authority. This daemon enforces "arrived via an address shaped like your tailnet," not "is actually a device you added to it."

If the `web-serve` tool is present on your `PATH` (coderig/agentsmith environments), `story web start`/`stop` additionally register/unregister the port with it — that tool's own access controls govern any exposure beyond what's described above.

### Security

Every request against `/api/**` requires a token — reads and writes alike, on every
interface including `127.0.0.1`. There is no exemption: opening the dashboard, browsing
a board, and every mutation all need a credential. Two kinds exist:

- **The master token** — one per daemon, minted at first start.

  ```bash
  story daemon token
  ```

  On a terminal that also copies it to your clipboard and says so on stderr; over SSH or
  Mosh it reaches the clipboard of the machine you're actually sitting at, via OSC 52.
  Piped or redirected, it prints the bare token and nothing else, so
  `TOKEN=$(story daemon token)` is safe. It authenticates everything, including the CLI's
  own control surface (`/api/v1/*`), and is never meant to reach a browser.

- **Named tokens** — one per device or tab, minted on purpose and revocable on purpose.

  ```bash
  story token new <name>      # mints and prints the raw secret once, to stdout only
  story token list             # shows every live token by name, never the secret
  story token revoke <name>    # ends one immediately
  ```

  Persistent (30-day default lifetime, survives a daemon restart) and authenticate
  everything the dashboard does — reads, project/story CRUD, dispatch — the same as the
  master token, but scoped to one holder you can name and revoke without touching anyone
  else's. Paste one into the dashboard's own token prompt and it never touches page
  JavaScript: the page posts it once to be exchanged for an `HttpOnly` cookie, which the
  browser then attaches automatically and no script can read.

**`story web open` arms a one-shot coupon instead of showing you a token.** Opening the
URL it prints redeems that coupon for a fresh named token — its own, individually
revocable via `story token revoke`, and shorter-lived (24 hours) than one you mint
yourself, since it was granted by a click rather than a deliberate `story token new`. Every
story or state change still fires that project's configured event hooks, and a hook is a
shell command — a named token reaches that surface exactly as the master token does. Full
reasoning: [`docs/spec/dashboard-authorization.md`](docs/spec/dashboard-authorization.md).

A named token travels two ways, held to different standards. An explicit
`X-Storyhook-Token` header is trusted outright — a page cannot forge one on a plain
navigation, and a cross-origin `fetch` that tries triggers a CORS preflight this daemon
never answers. The dashboard's own cookie is ambient instead, so a **read** authenticated
by the cookie additionally requires the browser-supplied `Sec-Fetch-Site: same-origin`
header, which no page can forge or suppress: `SameSite=Strict` alone doesn't distinguish
this dashboard's own tab from any other tailnet peer's, because cookies ignore port and
every peer under one tailnet is same-site with every other. A **mutation** needs no extra
check here, because it has already passed the guard below.

Mutating requests (creating or deleting a project; creating, moving, editing, or deleting a story) additionally require:

- a same-origin request — the dashboard's own page sets a custom `X-Storyhook` header that a cross-site request cannot replicate without triggering a CORS preflight the server never answers;
- a `Host` header that resolves to `127.0.0.1`, `localhost`, `::1`, the tailnet IP this instance bound itself, or — when Tailscale MagicDNS is on — this machine's full MagicDNS name (e.g. `host.tailXXXXX.ts.net`) — this is what stops DNS-rebinding, which the header check alone can't catch. The bare short hostname (`host`, without the `.ts.net` suffix) is deliberately *not* trusted: unlike the full name, it can resolve through a DNS search domain that isn't your tailnet's, so trusting it could reopen the exact rebinding this check exists to stop.

These two checks alone are not authentication — anything that can set two headers directly (a `curl` from any peer your tailnet lets reach the dashboard's bound IP) passes both with no credential at all. They defend a *browser* being tricked into sending a request on a victim's behalf; the token above is what actually establishes who is asking. (Full design and review: [`docs/spec/dashboard-authorization.md`](docs/spec/dashboard-authorization.md).)

`GET /` is the one exception, reachable with no token at all: it serves the dashboard's own page, which is what prompts for a token in the first place. `GET /api/events` (the live-update stream) needs a token like every other read — a same-origin `EventSource` carries the dashboard's own cookie automatically, so there is no `?token=` query parameter to fall back on, and no other route accepts one that way either.

### Reverse-proxying the dashboard

**Set `STORYHOOK_WEB_TRUSTED_HOSTS` before you put any reverse proxy in front of this
daemon.** It widens the `Host` allowlist so mutations work under the proxy's own
hostname, which a request arriving with `Host` rewritten to the proxy's name would
otherwise fail as a DNS-rebinding attempt:

```bash
STORYHOOK_WEB_TRUSTED_HOSTS=my-proxy-host story web start
```

Only list hostnames that are themselves no more exposed than your tailnet.

## Automation and scripting

`story` is designed to be used by shell scripts and coding agents.

Global flags:

- `--json` emits a structured JSON response envelope
- `--quiet` suppresses normal success output
- `--no-hooks` skips this command's git hooks
- `--store-path <file>` names the store file for this command, overriding `$STORYHOOK_STORE_PATH`
  and `$STORYHOOK_DATA_DIR` (see [Storage model](#storage-model))
- `--project <slug>` names the project for this command, overriding `$STORYHOOK_PROJECT` and the
  directory-based resolution below
- `--deadline <secs>` bounds how long this command waits on the daemon before giving up (`0`
  gives up immediately)

Exit codes:

- `0` success
- `2` usage or validation error
- `3` not found
- `4` lock timeout
- `5` integrity or storage error

Examples:

```bash
story show SH-1 --json
story list --flagged --json
story move SH-2 done --quiet
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

`STORYHOOK_DAEMON_ADDR` chooses a **port**, and `127.0.0.1` is the only IP it
accepts — any other one is refused with an error rather than accepted and
ignored, because the daemon binds loopback (plus your tailnet interface, when
you have one) and has no other address to offer. `--port N` says the same thing
without naming an address. To reach the dashboard from another machine, use the
tailnet address the daemon already binds for you: a wider bind would put a
full-privilege API on the local network.

`story` also refuses, on its own, to create a project at a path under a
temporary directory when the store it would write to is not itself temporary —
a backstop for a suite that built a fixture but forgot to name a store at all.
It is not a substitute for isolating deliberately: it only catches the shape
above, not every way a command can reach the wrong store.

It also refuses once too many projects appear in a real store too fast: 5 or
more inside ten minutes stops at the 5th, whether or not any of them named a
temporary path — a suite that forgets isolation entirely, or that builds
fixtures somewhere other than `$TMPDIR`, gets a loud stop after 4 junk
projects instead of a silent one after hundreds.
`STORYHOOK_ALLOW_PROJECT_BURST=1` overrides it for the rare case where the
volume really was on purpose.

## AI agent integration

Three commands support AI coding agent workflows:

- `story context` -- generates a project overview document (states, priorities, relationships, and ready work) suitable for the start of an AI session. Use `--format json` for structured output.
- `story next` -- surfaces the highest-priority unblocked story so an agent can pick up work without manual triage. Use `--count <n>` for the sequential execution order: each result virtually completes before the next is chosen, allowing a dependency-blocked story to appear after its blocker. This is a pure read; when you are about to start work on the answer -- and always when more than one agent may be running against the same project at once -- use `story claim --next` instead, which selects and takes in one write transaction.
- `story claim (<id> | --next)` -- takes a story to work on, atomically. One of the two forms is required and they are mutually exclusive: a bare `story claim` is refused rather than resolved to `--next`, because this writes, and a script whose id argument came out empty must not silently claim whatever happened to sort first. The move into the project's active state happens inside one write transaction either way, so two callers racing it are handed two different stories rather than one winner and a corrupt second claim; a story somebody else already holds is answered with `result:"conflict"` and `.actual` naming the state found. A claim comments by default (`--comment <text>` replaces the text, `--no-comment` posts none), in the same transaction as the claim itself, and `--dry-run` reads for real while writing symbolically.
- `story unclaim <id>` -- hands a claim back. The inverse of `story claim`, and the store half of it: the state change and its comment, never a tmux window and never a worktree. The story returns to the state it was claimed **from**, derived from its own event log inside the same write transaction rather than stored anywhere — so no caller has to carry the answer around. When that state cannot be restored (the story was created directly in the active state, or that state has since been removed or reclassified CLOSED) it returns to `todo` instead, and the substitution is reported in the result and written into the default comment rather than performed silently. A story somebody else has already moved is answered with `result:"conflict"` and `.actual` naming the state found.
- `story handoff --since <duration>` -- generates a session handoff document summarizing what changed during a work session (e.g. `--since 2h`). Useful when passing context between agents or between an agent and a human.

### MCP server

`story mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io) server on
stdin/stdout, exposing a curated set of eighteen tools (`story_list`, `story_next`,
`story_claim`, `story_new`, `story_move`, and so on — `story help mcp` lists them all) to an
agent host that speaks the protocol, over the same `/api/v1/invoke` door every other client
uses. A tool call is exactly as safe, and exactly as visible in a story's write history, as
the equivalent typed command. `story_claim` and `story_unclaim` are how an agent takes and
hands back work atomically; `story_next` stays a pure read. Every tool names its own `project` explicitly, since a long-lived server process has
no working directory of its own to infer one from. See `docs/spec/mcp-server.md` for the
design, including why this is not the first time storyhook has shipped one.

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
