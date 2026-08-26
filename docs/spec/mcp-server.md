# The MCP server: `story mcp`

Design of record for **SH-340**. A reversal of a recorded decision, not a new feature in a
vacuum — read "Why this is a reversal, not a cleanup" before anything else here.

## Why this is a reversal, not a cleanup

storyhook shipped an MCP (Model Context Protocol) server once already. `src/mcp.rs` (732
lines, protocol revision `2024-11-05`) and `src/mcp_install.rs` existed from the project's
earliest releases and were **deliberately deleted** on 2026-04-07, by commit `c32b117`
(story SH-32, plan at `.planning/PLAN.replace-mcp-with-docs.md`), and replaced with `story
help --compact` injected by the plugin's `SessionStart` hook — the integration that was, until
this story, the only one in place.

`tests/mcp_removal.rs` (311 lines) exists specifically to catch a regression back to that
design. This story keeps every one of its assertions green — not by weakening them, but
because the new server is spelled differently from the old one everywhere that file checks:
a subcommand (`story mcp`) rather than a flag (`--mcp`); no `mcp-config` verb; no MCP text
injected into `story help --compact`, the scaffold templates, generated `AGENTS.md`, or
`session-start`'s output; and the module lives at `src/mcp/mod.rs`, not `src/mcp.rs`, so the
literal file the old test path-checks for still does not exist. What that file actually
guards — "the *old* design's exact surface never comes back" — remains true and enforced.
What follows is the argument for why a *new* design earns a second chance.

### What killed v1

A post-mortem of the deleted file shows the failure precisely. Of its 732 lines, roughly 100
were JSON-RPC framing — parsing a request, dispatching by method, writing a reply — and
nothing about those has ever needed to change. The other ~630 were eighteen hand-written JSON
Schema literals describing each tool's arguments, plus a hand-written `build_invocation`
function mapping a tool call's JSON onto an `Invocation` by hand, field by field. That is a
**second, parallel copy of the CLI's command surface**, kept in sync by a human remembering
to update it. Two stories — SH-9 and SH-17 — exist in this tracker's own history for no
purpose but re-syncing one field (`story_type`) into those schemas after it was added to the
CLI and forgotten in the MCP copy. The framing was never the problem. The duplication was.

On top of that, v1's `handle_tools_call` called `app::run` directly — a second, in-process
copy of the whole application, executing against the store from inside the MCP process. That
function no longer exists; W6 deleted 10,849 lines including it. There is no way to build a
2026-era MCP server that way even if this story wanted to.

### What changed since April 2026

The whole reason v1's design was survivable at all — briefly — is that in April 2026 there
was no daemon and no `/api/v1/invoke`. Today there is exactly one door every `story` command
reaches the store through (SH-114), and the CLI's own `HttpInvoker` (`src/invoke.rs`) already
embodies the retry doctrine, the daemon-discovery and liveness logic, and the two-step
confirmation flow every client of that door needs. An MCP server built in 2026 is not a
second implementation of storyhook — it is a **third client of an existing one** (the CLI and
the web dashboard being the first two), exactly the way the dashboard is. That is what makes
this cheap now and made it expensive then.

### What makes v2 structurally different, not just better-intentioned

`Invocation` (`src/cli.rs`) is `Serialize + Deserialize`, externally tagged, with every field
a `String`, `bool`, `usize`, `Vec<String>` or `Option` of one of those — it is the wire
envelope's request half, and has been since W0b. `cli::parse_invocation` is `pub fn`. This
server never builds an `Invocation` by hand: every tool assembles an `argv` — the same shape
`std::env::args()` hands `main` — and calls `cli::parse_invocation` on it, the *exact*
function the real binary calls for a typed command. There is only one parser. A tool's
argument table can drift from what that parser accepts, but it cannot silently *misconstrue*
one, because nothing in this module ever decides what an `Invocation`'s fields mean —
`parse_invocation` does, the same as it always has. See `src/mcp/tools.rs`'s module doc and
"Anti-drift mechanism" below.

## Decision: hand-rolled, not the `rmcp` SDK

Decided by a 3-member `/council-vote` (software-architect, api-designer, skeptic), unanimous
3-0 in round 1 before any member saw another's reasoning. Its audit trail was untracked
and is gone (SH-363), and it belonged to no story, so what follows is the whole of the
record.

`story mcp` is implemented on the crate's existing blocking `std::io`, adding zero new
dependencies — not on `rmcp` (the official Rust MCP SDK), which would add tokio and roughly
twenty crates to the one binary this project ships. Two independent reasons:

1. **`rmcp` does not prevent v1's actual failure.** Its `schemars`-derived JSON Schemas come
   from *new* Rust structs a server author writes for each tool's arguments — which then
   still need a hand-written mapping into `Invocation`. That is `build_invocation` again,
   with only the schema-authoring half automated; the half that actually rotted (the
   mapping) is unchanged. This server's schemas are generated from one shared table
   (`src/mcp/tools.rs::json_schema`) and construction goes through the real parser, which
   `rmcp` cannot give for free because it has no notion of this crate's own `Invocation`.
2. **This codebase has already decided against an async runtime, deliberately, in writing.**
   `Cargo.toml`'s own comment: *"this crate has no async runtime and wants none: every I/O
   path here is blocking (`ureq`, our own HTTP/1.1 layer)."* SH-259 chose `zbus`'s
   `rt-async-io` feature over `rt-tokio` specifically to keep a second reactor out of the
   Linux release target. `rmcp`'s `server` feature would put one there for a single stdio
   client whose every tool call ends in a blocking `HttpInvoker::invoke` call anyway — no
   concurrency is bought, only an async runtime this project has already said it does not
   want.

Only one deliberation item survived to the winning proposal (all three seats reached it
independently, in the vote round): the server must be a **stateless per-call bridge**.
`src/main.rs` collapses `$STORYHOOK_PROJECT`, `$STORYHOOK_ACTOR`, and hook depth exactly once,
at process start, specifically because a daemon's environment belongs to whoever happened to
start it — not the caller of the moment. A long-lived MCP server reading those from its own
environment would be that exact mistake recurring one layer out: every tool call for the rest
of the session would be silently labeled with whatever the *host* had set at launch, rather
than what the caller of that specific call meant. This server reads neither. Every tool takes
`project` (required) and `actor` (optional, on writes) as explicit JSON arguments instead.

## What was decided that the vote did not settle

Two small implementation calls, made during the build, recorded here rather than as separate
questions:

- **`ProjectSelector` reuses the existing `Flag` variant** rather than growing a third one.
  The dashboard's own REST layer already does this (`src/api/rest.rs`) for exactly the same
  reason: `Flag` means "named explicitly, right now, by this caller" — true of a `--project`
  flag, a dashboard click, and an MCP tool argument alike. `Environment`'s problem is a value
  set once and silently inherited forever, which is not the shape of a per-call tool
  argument. Reusing the existing type is what "prefer reuse over new code" means here.
- **No `structuredContent`.** A `CallToolResult` may carry a machine-readable
  `structuredContent` object alongside its `content` text, validated against a declared
  `outputSchema`. Adding one would mean authoring and maintaining a *second* schema per tool —
  exactly the kind of surface this design exists to avoid. `content` alone, carrying the same
  bytes `story --json` would print (`output::render_response`), is a complete, spec-valid
  result and is what this server sends.

## Architecture

```
story mcp  (stdio; a mode of the one `story` binary, dispatched in src/main.rs
            ahead of parse_invocation, beside `tui` and the foreground daemon serve mode)
      │
      ▼
src/mcp/mod.rs        module doc + McpServer's public surface
src/mcp/protocol.rs   JSON-RPC 2.0 framing: parse one line, one reply (or none) per line
src/mcp/server.rs     method dispatch: initialize, ping, tools/list, tools/call
src/mcp/tools.rs       the tool table, JSON Schema generation, argv construction
      │
      ▼  tools/call: build an argv, call cli::parse_invocation — the CLI's own parser
InvokeRequest { invocation, project: Flag { slug }, actor }
      │
      ▼
HttpInvoker::invoke(request)     ← src/invoke.rs — the CLI's only door since SH-114
      │   daemon discovery/spawn, the portfile token, the record-polling
      │   liveness bound, and the SH-312 retry doctrine — inherited, not
      │   re-implemented: retry only a refused connection; report every
      │   other outcome, timeouts included, as "may or may not have run"
      ▼
POST /api/v1/invoke  →  the daemon  →  Response
      │
      ▼
content: [{ type: "text", text: render_response(&r, json=true, quiet=false) }]
isError: false                    (or true, with output::render_error's text, on AppError)
```

### Why this cannot be a second binary

`daemon::lifecycle::DaemonInfo::is_this_binary` (`src/daemon/lifecycle.rs`) identifies a
usable daemon by comparing the daemon's recorded executable **path and modification time**
against the calling process's own `current_exe()`. A separate `story-mcp` binary would never
match, would fall to `spawn_locked`, and would **evict the daemon the real `story` binary is
using** — after which the CLI's next command would evict that one back. Two binaries would
fight over the daemon forever. `story mcp` is therefore a mode of the one shipped binary,
exactly like `story tui` and `story daemon --serve` already are, dispatched in `main.rs`
before `parse_invocation` ever runs — never a new `Invocation` variant, never a second
artifact in the release matrix.

### The five obligations of any client of this daemon

All five apply to `story mcp` because it is a client, and a long-lived one sharpens three of
them:

1. **Ambiguity is reported as ambiguity, never as failure.** CLAUDE.md's SH-312 doctrine
   binds "any client this daemon has, present or future." Inherited for free by calling
   `HttpInvoker` rather than hand-rolling the POST.
2. **Nothing is read from this process's own environment.** `project` and `actor` are
   explicit tool arguments, per the council's decision above.
3. **Hook depth travels in the request**, resolved once at process start (the same way an
   ordinary command resolves it) and passed to every call — never re-read per call, and never
   inferred from ambient state that could drift mid-session.
4. **The two-step protocols are never auto-forced.** `Response::ConfirmationRequired` and
   `Response::SetupRequired` are unreachable by this story's sixteen tools today (their
   triggers — `Purge`, an unforced `Reopen` of a soft-deleted story, `HideState`,
   `GithubSync`'s first-run setup — are none of the sixteen), but `server.rs` handles both
   defensively anyway: either produces a tool result with `isError: true` explaining that
   this transport does not support the interactive confirmation the CLI would ask for, and
   naming the equivalent `story` command to run from a terminal instead. Nothing is ever
   forced through automatically.
5. **A mutation deadline, if this server ever needs one, is derived from
   `event_hooks::HOOK_TIMEOUT_CEILING_SECS`, never hand-copied** — the SH-136/SH-312 lesson.
   Not yet needed: `HttpInvoker`'s own record-polling liveness bound already covers a tool
   call's wait, the same as it covers a CLI command's.

## The tool surface

Eighteen tools, curated to the loop an agent actually runs — not a 1:1 mirror of the CLI's 64
`Invocation` variants. `story help mcp` lists them; `src/mcp/tools.rs::TOOLS` is the
authoritative table, and `tests/mcp_tool_drift.rs` derives the help topic's list from it in
both directions rather than letting the two be kept in step by hand. Every tool takes
`project` (required) and, on a write, `actor` (optional) — never a working directory, since a
stdio session has none of its own to offer.

Deliberately absent: anything that asks a human to confirm interactively (`purge`,
`hide-state`, an unforced `reopen` of a soft-deleted story), `github-sync` and its first-run
setup wizard, bulk/import/export operations, and administrative surfaces (`daemon`, `token`,
`store`, `plugin`). All of these remain reachable through the CLI; a later story can widen the
tool surface following the same anti-drift discipline if a real workflow needs one of them.

### `story_claim` and `story_unclaim`, and why their defaults disagree (SH-479)

`story_claim` is the reason an MCP agent can take work at all. Before it, the only writing
tool that could move a story was `story_move`, which cannot compare-and-swap on the state it
just read without a round trip in between — precisely the race `story claim` exists to close,
and MCP agents are exactly the population running several sessions against one project.
`story_next` stays a pure read and points at `story_claim` for the taking case, mirroring the
pointer `story_list` already carries toward `story_next`.

The two tools disagree about what an omitted `comment` means, and the disagreement is
**derived, not chosen**:

| | omitted `comment` builds | who composes the default |
|---|---|---|
| `story_claim` | `--no-comment` | the client — and this server is the wrong client |
| `story_unclaim` | nothing at all | the store, which owns the facts |

A claim's default sentence names the *caller's* host and tmux window, so
`cli::ClaimComment::Default` is resolved client-side by `claim_comment::resolve` and a
`Default` that reaches the daemon is refused outright (`claim_comment::UNRESOLVED_REFUSAL`).
Emitting no flag would therefore make this tool's most common call fail every time with an
`internal:` message no agent can act on — measured, not argued: deleting the `--no-comment`
arm makes `tests/mcp_server.rs`'s round-trip test fail with exactly that refusal at exit 2.

Resolving it here instead was rejected. `resolve` reads this process's own `$TMUX` and
hostname, and this server is long-lived and started by an agent host, so it would name
whichever shell happened to launch it, on every call, forever — the SH-246 mistake one layer
out, which the "No ambient state" scan below already forbids for `project` and `actor`. SH-490
reached the identical conclusion for `story.sh dispatch`'s own claim, on the identical
grounds: a caller with nothing honest to say says nothing, and a fabricated window is never
acceptable. An agent that wants the claim to record who took the story passes `comment`.

An unclaim's default names the state the story is being restored to and whether that was a
fallback — two facts that do not exist until the write transaction is already open. It travels
to the store and is composed there, so it arrives over MCP intact. This is the payoff of
SH-483's decision to keep `ClaimComment` and `UnclaimComment` as two types that share a shape
and never a contract: one enum would have made this asymmetry unstatable.

Three further boundaries, all deliberate:

- **`dry_run` is on neither tool.** An MCP caller that wants to know what a claim *would* do
  calls `story_next` or `story_show`.
- **`story reset` is not reachable over MCP at all**, and `story_unclaim`'s description says
  so. It is git and tmux mechanics living in `plugins/story/bin/story.sh` (SH-484), the same
  reason `dispatch` and `reap` have never been tools. `story_unclaim` releases the claim in
  the tracker; a worktree and a tmux window created for the story survive untouched.
- **`build_claim` states exactly one cross-field rule in its own vocabulary** — exactly one of
  `id` or `next`, never both, never neither — because `parse_claim`'s usage line answers about
  `<id>` and `--next`, which are not what an MCP caller typed. Everything else (`phase` beside
  an explicit `id`) is relayed from the parser, whose own message names the thing the caller
  got wrong.

## Anti-drift mechanism

Five checks, all in `tests/mcp_tool_drift.rs` and `src/mcp/tools.rs` unless noted, aimed
squarely at the failure that killed v1 — a tool's declared shape silently drifting from what
the command it drives actually accepts:

1. **Exhaustive compile-time fence.** `tools::tool_for_variant` is a `match` over every
   `Invocation` variant with no wildcard arm. A 65th variant is a compile error here until
   someone has decided whether it gets a tool — the same discipline `src/api/routes.rs`'s
   router already uses for HTTP routes.
2. **Same-answer-two-doors.** For each tool, the test constructs representative arguments,
   builds the `Invocation` the tool's own `build_argv` + `cli::parse_invocation` produces, and
   separately hand-writes the equivalent CLI `argv` and runs it through
   `cli::parse_invocation` independently — asserting the two `Invocation`s are equal. Because
   both paths already converge on the same function, this is a regression guard *by
   construction* rather than by coincidence — but it is still the test that would catch a
   tool's argument table quietly falling out of step with what its `build_argv` actually
   sends.
3. **No second schema.** `json_schema` in `src/mcp/tools.rs` is the *only* place any `src/mcp/`
   file constructs a `"properties"` key. `tests/mcp_tool_drift.rs` counts occurrences of that
   literal across `git ls-files -- 'src/mcp/*.rs'` and fails above one — the guard against a
   second, hand-written schema for one tool ever being added beside the shared builder, which
   is the exact shape of the SH-9/SH-17 failure.
4. **No ambient state.** A source-text scan over the same file set forbids `std::env::var` and
   `current_dir` anywhere under `src/mcp/` — the stateless-bridge contract made structurally
   unrepresentable rather than merely documented.
5. **One list of tools, not two** (SH-479). `story help mcp` is how a host without
   `tools/list` learns what exists, and its `Tools:` block was a hand-kept copy of `TOOLS`.
   `every_curated_tool_is_listed_in_the_mcp_help_topic` derives both sides and compares them
   as sets in both directions — a curated tool the topic never mentions, and a topic entry the
   table no longer declares, each fail by name.

## As built

- **`ProjectSelector::Flag` is reused rather than a new variant added** — see "What was
  decided that the vote did not settle" above.
- **No `structuredContent`** — same section.
- **Sixteen tools, not "~15"** — `story_label` and `story_unblock`/`story_block` as a pair
  rounded the curated set to sixteen; the number was always approximate in the plan that
  authorized this story. SH-479 later took it to eighteen with `story_claim` and
  `story_unclaim`.
- **`story help mcp`'s tool list stopped being hand-maintained** (SH-479). It was a second
  copy of `TOOLS` kept in step by whoever remembered — the shape SH-136/SH-198/SH-258/
  SH-260/SH-360 have already cost this project five times over, and it was noticed while
  adding two entries to it. `tests/mcp_tool_drift.rs`'s
  `every_curated_tool_is_listed_in_the_mcp_help_topic` now compares the two as sets in both
  directions, reading only the topic's `Tools:` block — never the whole topic, since
  `story_new`, `story_prioritize` and `story_context` are all named again under `Related:`,
  where a scan would report a tool as listed after it had been dropped from the list a reader
  actually reads.
- **`verb_is_recognized` in `src/cli.rs` gained one more special case** (`verb == "mcp"`,
  beside the existing `verb == "tui"`), so `story mcp --help` explains the command via its
  help topic rather than the parser reporting an unknown command — exactly `tui`'s own
  precedent, reusing `help_for_verb`'s existing topic lookup rather than adding a second
  help-dispatch path.
