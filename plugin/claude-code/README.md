# `story` — the storyhook Claude Code plugin

A story lifecycle toolkit for [storyhook](https://github.com/mikeydotio/storyhook).
Install with `story plugin install claude-code`, or `/plugin marketplace add
mikeydotio/storyhook` then `/plugin install story@storyhook`.

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

`bin/story.sh` accepts seven subcommands:

| Subcommand | Verb it backs |
|---|---|
| `list` | bare `/story` |
| `view <id>` | `/story view`, `/story <id>` |
| `dispatch <id> [--auto]` | `/story do` |
| `create --title …` | `/story new` |
| `complete <plan\|execute> <id>` | `/story complete` |
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

- **`repo_root()` uses `--git-common-dir`, not `--show-toplevel`.** Run from
  inside a dispatched worktree, `--show-toplevel` would return that worktree's
  root and the next worktree would be nested inside it.
- **Every `story` call goes through `story_cli()`,** which anchors to the
  project root. The CLI resolves a project by walking up from its working
  directory and every checkout of a repository resolves to the same project, so
  this is no longer what stands between you and the wrong tracker (SH-46). It
  is what stands between you and *no* project, or a neighbouring one: these
  verbs can be invoked from anywhere, including outside the repository.
- **`is_ready()` is not "unclaimed".** It returns true for an already
  in-progress story, so `dispatch` carries its own guard and `list` filters the
  active state out.
- **The claim is a hard precondition, not a trailing best-effort.** Storyhook's
  `state` *is* the claim marker, so `dispatch` claims via `--if-state` CAS
  before any side effect, and rolls the claim back if a later step fails.
- **`complete` never forces anything.** `git worktree remove` runs without
  `--force`, and a branch is deleted only if merged into `origin/<default>` or
  local `<default>`; an un-comparable branch counts as *not* merged.
- **`story doctor` exits 5 when it finds anything** and emits `.issues[]` only
  when that array is empty, so `/story doctor` treats a non-zero exit as a
  finding, never as a failed probe.
- **An `--auto` session closes with a plain `story move`, never `/story
  complete`.** `complete` asks a confirming question — fatal to an unattended
  run — and would try to remove the very worktree the auto session is
  standing in; teardown stays a later `/story complete <id>` from the main
  checkout, same as the attended path.

## Environment

All knobs are `STORY_*`. The commonly useful ones:

| Variable | Effect |
|---|---|
| `STORY_DRY_RUN=1` | preview any side-effecting verb; changes nothing |
| `STORY_LAUNCH_CMD` | what `dispatch` launches (must **not** include `-w`) |
| `STORY_PROMPT` / `STORY_PROMPT_EXTRA` | the handoff prompt, and a clause appended to it |
| `STORY_AUTO_PROMPT` | the `--auto` charter (same seam as `STORY_PROMPT`, autonomous runs only) |
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
