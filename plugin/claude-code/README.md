# `story` — the storyhook Claude Code plugin

A story lifecycle toolkit for [storyhook](https://github.com/mikeydotio/storyhook).
Install with `story plugin install claude-code`, or `/plugin marketplace add
mikeydotio/storyhook` then `/plugin install story@storyhook`.

**Requires the store-backed `story` CLI (1.0+).** Since plugin 0.4.0, story data
lives in storyhook's global store behind a local daemon — not in a `.storyhook/`
directory — and the plugin's enable/tracking switches live in the repository's
`.storyhook.toml` under `[plugin]` (they moved from `.storyhook/plugin-config.toml`).
Repositories still carrying a `.storyhook/` tree migrate once with `story migrate`.

## Layout

```
skills/story/SKILL.md     the router — parses the verb, renders results
skills/story-*/SKILL.md   nine standalone skills, each independently invocable
bin/story.sh              ALL deterministic work; one JSON object per run
lib/session.sh            vendored tmux/worktree/readiness/git-safety core
references/               protocols the router loads on demand
hooks/                    SessionStart, PostToolUse (git), Stop
tests/                    plain-bash suite, wired into the repo's `make test`
```

## Design

**The skill is a thin router.** Everything with a side effect — tmux, git
worktrees, the readiness gate, the storyhook claim — lives in `bin/story.sh`,
which always emits exactly **one JSON object** on stdout carrying `ok` and a
human-readable `display`. The model routes and renders; it never drives `story`,
`git`, or `tmux` itself. That is what keeps behavior testable: the bash suite
exercises the real logic, and the prose can't quietly diverge from it.

`bin/story.sh` accepts eight subcommands:

| Subcommand | Verb it backs |
|---|---|
| `list` | bare `/story` |
| `view <id>` | `/story view`, `/story <id>` |
| `dispatch <id> [--auto]` | `/story do` |
| `create --title …` | `/story new` |
| `complete <plan\|execute> <id>` | `/story complete` |
| `reap <id>` | not routed by the skill (SH-208) — the `--auto` charter's own final act; see below |
| `capture <id>` | `/story capture` |
| `doctor` | `/story doctor` |

## Provenance

`lib/session.sh` is a vendored fork of agentics' `plugins/issue/lib/session.sh`;
`dispatch` is forked from agentics' `plugins/storywork`, and `complete`, `view`,
`list`, `create`, `doctor`, and `capture` from `plugins/issue`. Each carries a
provenance header naming its source and the date. The fork is deliberate — this
plugin must be installable on its own, with no cross-marketplace dependency —
so expect it to drift from the originals over time.

## Things that are load-bearing

Worth knowing before changing anything here:

- **The repo-side verbs take their directory from the project, not from the
  caller.** `dispatch`, `capture` and `complete` ask `story project show` where
  the project's checkout is, and `dispatch`/`complete` then `cd` there once, for
  real, before any git call. That is the whole of SH-120: every git invocation in
  `bin/story.sh` and `lib/session.sh` is bare — no `-C` — so before the `cd` each
  one acted on whatever repository the caller happened to be standing in, and a
  `/story do` run from the wrong checkout cut its worktree there.
- **The slug is pinned before the `cd`, not after.** `resolve_checkout` sets
  `$PROJECT_SLUG` from the same `project show` response that gave it the path.
  Resolving again from the new directory would be a different question: in a
  monorepo a sub-project owns its own identity and is entitled to answer
  differently (SH-151).
- **`repo_root() <dir>` uses `--git-common-dir`, not `--show-toplevel`.** Given a
  dispatched worktree, `--show-toplevel` would return that worktree's root and
  the next worktree would be nested inside it. It answers "where does worktree
  bookkeeping happen?", never "which project is this?", and its `<dir>` argument
  is required precisely because the default it would otherwise take is the
  caller's working directory.
- **`story_cli()` does not choose a project.** The CLI does, and it knows things
  a shell walk cannot: `$STORYHOOK_PROJECT`, and whether a repository's origin
  is registered. `story_cli` used to `cd` to `repo_root()` first, which
  *overrode* that — in a monorepo with a project at the top level and another in
  a subdirectory, `view` from the subdirectory reported "not found" and `list`
  listed the root project's stories, silently, while the CLI standing in the
  same place answered correctly (SH-121).
- **A checkout that cannot be used is refused before the claim.** No linked
  checkout, a recorded path that is gone, a directory that is not a git
  repository, or a checkout recorded as a *linked worktree* — each names the way
  out, and each lands ahead of `dispatch`'s compare-and-swap claim so a refusal
  never strands a story at in-progress. A worktree that is not where the checkout
  says it should be is reported, never reconstructed from the caller's own
  repository.
- **`--project <slug>` is story.sh's own global option**, stripped before the
  verb and forwarded to every `story` call. It is what makes the read verbs
  usable outside a repository.
- **A failure is never defaulted away.** `list` used to fall back to
  `{"stories":[]}` on any error, so a refusal read as "No ready stories to pick
  up" — an empty answer for the tool whose job is handing out work (SH-163).
  `_load_ready_stories` fails loudly and carries the CLI's own diagnostic.
- **`is_ready()` is not "unclaimed".** It returns true for an already
  in-progress story, so `dispatch` carries its own guard and `list` filters the
  active state out.
- **The claim is a hard precondition, not a trailing best-effort.** Storyhook's
  `state` *is* the claim marker, so `dispatch` claims via `--if-state` CAS
  before any side effect, and rolls the claim back if a later step fails.
- **`complete` never forces anything.** `git worktree remove` runs without
  `--force`, and a branch is deleted only if merged into `origin/<default>` or
  local `<default>`; an un-comparable branch counts as *not* merged.
- **`story doctor` exits 5 when it finds anything**, so `/story doctor` treats a
  non-zero exit as a finding, never as a failed probe. A healthy run answers
  `.findings[]` (empty) and `.advice[]`; a damaged one fails with the same
  `.findings[]` populated, each carrying a `code`, the story it concerns, and —
  for a read-model divergence — the `field`/`persisted`/`rebuilt` values that
  used to be readable only by regexing `.error` (SH-244). `.issues[]` is the
  deprecated spelling of `.advice[]`.
- **An `--auto` session closes with a plain `story move`, never `/story
  complete`.** `complete` asks a confirming question — fatal to an unattended
  run — and would try to remove the very worktree the auto session is
  standing in. Once closed, it runs `story.sh reap <id>` as its own last act
  (SH-208): reclaims the worktree and branch, then kills the tmux window it
  was running in. `reap` refuses outright unless the story is closed and the
  worktree/branch are both safe to discard — nothing partial, matching
  `complete`'s own "never forces anything" rule above but all-or-nothing
  rather than best-effort, since nobody is watching to read a partial
  result. The attended path is unchanged: teardown there still stays a later
  `/story complete <id>` from the main checkout.
- **`--auto` renders one of TWO charters, decided by `story.sh` itself, not
  left to the child's own guess** (SH-219). `council_vote_available` probes
  for a real `skills/council-vote/SKILL.md` — bare under `~/.claude/skills`
  or the project's own `.claude/skills`, or shipped by an enabled entry in
  `installed_plugins.json` — before the charter is ever rendered. Found: the
  charter convenes `/council-vote` for a genuinely hard decision. Not found:
  it says so, and tells the child to research, decide, and record instead —
  never to stall waiting on a skill that was never going to answer. Either
  charter tells the child that an *easy* decision (one clear best answer)
  gets researched and decided on its own, full stop — `--auto` was never
  meant to route every open question through a mechanism at all.

## Environment

All knobs are `STORY_*`. The commonly useful ones:

| Variable | Effect |
|---|---|
| `STORY_DRY_RUN=1` | preview any side-effecting verb; changes nothing |
| `STORY_LAUNCH_CMD` | what `dispatch` launches (must **not** include `-w`) |
| `STORY_PROMPT` / `STORY_PROMPT_EXTRA` | the handoff prompt, and a clause appended to it |
| `STORY_AUTO_PROMPT` / `STORY_AUTO_PROMPT_SOLO` | the two `--auto` charters — council-available and no-council, respectively (same seam as `STORY_PROMPT`; either wins outright over the probe below) |
| `STORY_COUNCIL` | `auto` (default, probes for real)/`on`/`off` — which `--auto` charter `council_vote_available` picks |
| `STORY_DONE_STATE` | the state `complete` closes into |
| `STORY_TARGET_SESSION` | dispatch into a named session from outside tmux |
| `STORY_PROTECTED_BRANCHES` | extra globs `complete` must never delete |
| `STORY_REQUIRE_FRESH_BASE=1` | refuse to dispatch on a stale base instead of warning |

See `bin/story.sh`'s config block for the full list.

## Tests

```bash
bash plugin/claude-code/tests/run-tests.sh          # all
bash plugin/claude-code/tests/run-tests.sh complete # substring filter
```

They run against the **real** `story` binary (the repo builds it) and a real git
repo with a local bare origin under `/tmp` — deliberately not `$TMPDIR`, which
Spotlight indexes and which stalls file-intensive runs on macOS. Only `tmux` is
faked. `make test` builds the binary first and puts it on `PATH` for this suite,
so it always exercises the freshly built CLI rather than whatever is installed.
