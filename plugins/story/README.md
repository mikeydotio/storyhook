# `story` — the Storyhook agent plugin

A story lifecycle toolkit for [storyhook](https://github.com/mikeydotio/storyhook).
The same canonical skills and deterministic helper are packaged for Claude Code and Codex;
provider-specific manifests and adapters supply only the integration layer.

The Storyhook CLI supports both providers:

```bash
story plugin install claude
story plugin install codex
```

The Codex installer also creates `~/.codex/storyhook/story.sh` and a dedicated
`~/.codex/rules/storyhook.rules`. Skills call that unversioned launcher rather than their
versioned cache copy. The launcher delegates through `story plugin run codex`, which asks
Codex for the exact enabled plugin version on every invocation; upgrades therefore do not
stale the rule. The generated rule allows only `bash` plus the exact launcher path, is
verified with `codex execpolicy check`, and takes effect after Codex restarts. Bare `bash`
is never allowlisted.

The former plugin target `claude-code` remains accepted for install and uninstall as a
deprecated, warned compatibility alias. New dispatch interfaces accept only `claude` and
`codex`.

Codex development installs register the repository marketplace and then add the plugin,
using the following Codex commands internally:

```bash
codex plugin marketplace add /absolute/path/to/storyhook
codex plugin add story@storyhook
```

Running only those two low-level commands skips the launcher/rule lifecycle; use
`story plugin install codex` for the complete supported installation.

The Codex manifest declares the shared skills and intentionally has no explicit `hooks`
field because the current validator rejects it. Current installed-plugin discovery loads
`hooks/hooks.json` from the plugin root by convention. Those hooks use Codex's `PLUGIN_ROOT`
with Claude's compatibility variable as a fallback; a local non-managed plugin may require
an explicit trust/review step before Codex runs them.

**Requires the store-backed `story` CLI (1.0+).** Since plugin 0.4.0, story data
lives in storyhook's global store behind a local daemon — not in a `.storyhook/`
directory — and the plugin's `enabled` switch lives in the repository's
`.storyhook.toml` under `[plugin]` (it moved from `.storyhook/plugin-config.toml`).
Repositories still carrying a `.storyhook/` tree migrate once with `story migrate`.

## Layout

```
skills/story/SKILL.md     the router — parses the verb, renders results
skills/story-*/SKILL.md   nine standalone skills, each independently invocable
adapters/                 provider-specific dispatch and lifecycle instructions
bin/story.sh              ALL deterministic work; one JSON object per run
lib/session.sh            vendored tmux/worktree/readiness/git-safety core
references/               protocols the router loads on demand
hooks/                    provider lifecycle hooks
tests/                    plain-bash suite, wired into the repo's `make test`
```

## Design

**The skill is a thin router.** Everything with a side effect — tmux, git
worktrees, the readiness gate, the storyhook claim — lives in `bin/story.sh`,
which always emits exactly **one JSON object** on stdout carrying `ok` and a
human-readable `display`. The model routes and renders; it never drives `story`,
`git`, or `tmux` itself. That is what keeps behavior testable: the bash suite
exercises the real logic, and the prose can't quietly diverge from it.

On Claude, `<story-helper>` is the packaged `bin/story.sh`. On Codex it is the stable
`~/.codex/storyhook/story.sh` launcher installed by the CLI. That one provider-specific
indirection is a sandbox boundary: command rules match exact prefixes, while Codex plugin
cache paths include the version. The launcher dynamically resolves the enabled cache entry
and then runs the same packaged helper, preserving the one-JSON-object contract.

`bin/story.sh` subcommands:

| Subcommand | Verb it backs |
|---|---|
| `list` | bare `/story` |
| `view <id>` | `/story view`, `/story <id>` |
| `dispatch <id> [--auto] [--force] [--resume] [--agent=claude\|codex]` | `/story do`; `--resume` preserves and reconstructs an abandoned dispatch, while `--force` only reuses an existing claim for a fresh dispatch |
| `dispatch <id> --auto --full-auto [--force] [--agent=claude\|codex]` | engine-only lane launch; the dashboard, skills, and ordinary autonomous dispatch never add `--full-auto` |
| `dispatch --next [--auto] [--agent=claude\|codex]` | not routed by any skill (SH-344) — the id-less sibling: claims whatever `story claim --next` picks atomically, so a caller dispatching several stories at once (a fleet, a loop) gets a distinct story per call instead of racing the same id |
| `create --title …` | `/story new` |
| `complete <plan\|execute> <id> [--no-close] [--no-clean] [--force]` | `/story complete` |
| `reap <id>` | not routed by the skill (SH-208) — the `--auto` charter's own final act; see below |
| `unclaim <id> [--comment <t> \| --no-comment]` | `/story unclaim` (SH-484) — the inverse of `claim`: release the claim through `story unclaim`, then close the story's tmux window. Nothing on disk is touched |
| `reset <id> [--force] [--comment <t> \| --no-comment]` | `/story reset` (SH-484) — everything `unclaim` does, then deletes the worktree and the branch, for a story abandoned by a crash where restarting beats inheriting |
| `capture <id>` | `/story capture` |
| `doctor` | `/story doctor` |
| `ensure-cli` | the CLI-availability check six standalone skills used to hand-roll in prose |
| `context [--full]` | `/story-context` |
| `sync [--since <d>]` | `/story-sync` |
| `handoff [--since <d>]` | `/story-handoff` |
| `triage` | `/story-triage` |
| `scaffold-agents-md [--path <file>]` | shared setup's canonical `AGENTS.md` merge, including the project-new no-duplicate case |
| `scaffold-claude-md [--path <file>]` | the legacy Claude-specific `CLAUDE.md` integration |

The auxiliary lifecycle verbs (SH-308) route not through this skill but through their own standalone
skills (`skills/story-context`, `story-sync`, `story-handoff`,
`story-triage`, `story-setup`), and through `references/ensure-cli.md`, which six of the
nine standalone skills load — same one-JSON-object contract, same "route and render" rule,
different door in. `triage` is read-only: it gathers and classifies findings, but the
resolution commands (`prioritize`/`label`/`block`/…) stay direct `story` calls in the
skill, since each is already one unambiguous CLI invocation with nothing to parse.
Both scaffold helpers are sentinel-delimited insert-or-replace operations, never full
rewrites. `scaffold-agents-md` also recognises the exact canonical file already written by
`story project new`, so setup does not append a duplicate block immediately after project
initialization.

**`story-install` and `story-update` were deliberately left as prose (SH-308).** Both are
already about as deterministic as prose gets — single unambiguous commands (`command -v
story`, `story --version`, `story update --check`), no double-nested JSON to parse, and no
hand-copyable CLI default to drift. Wrapping `command -v story` in yet another script layer
would add indirection without removing any real ambiguity.

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
  never strands a story in the claimed state. A worktree that is not where the checkout
  says it should be is reported, never reconstructed from the caller's own
  repository.
- **`--project <slug>` is story.sh's own global option**, stripped before the
  verb and forwarded to every `story` call. It is what makes the read verbs
  usable outside a repository.
- **A failure is never defaulted away.** `list` used to fall back to
  `{"stories":[]}` on any error, so a refusal read as "No ready stories to pick
  up" — an empty answer for the tool whose job is handing out work (SH-163).
  `_load_ready_stories` fails loudly and carries the CLI's own diagnostic.
- **`unclaim` and `reset` differ only in what they may destroy, and their
  self-termination verdicts follow from that** (SH-484). Both release the claim
  through `story unclaim` (SH-483), which derives the state a story was claimed
  FROM out of its own event log and says so when it has to fall back to `todo`.
  `unclaim` touches nothing on disk, so called from inside the story's own tmux
  window it does the release and skips only the window kill, naming the skip —
  closing that pane would destroy the very answer the fallback rule exists to
  state. `reset` cannot skip its destructive step, and skipping the window kill
  instead would leave a live shell in a deleted directory, so it refuses
  outright, `--force` included.
- **`reset` asks whether work is RECOVERABLE, not whether it is MERGED.** Its
  three `--force`-able refusals — a dirty worktree, commits on no remote, a
  locked worktree — are one question wearing three hats, which is why there is
  one flag. Merged-ness is the wrong question here and is why `reap`'s
  `unmerged` veto is not reused: a branch pushed to `origin` and merged nowhere
  is fully recoverable, and a branch that only ever lived on this disk is not
  recoverable at all, yet `git branch -d` calls both unmerged.
  `protected-branch`, `self-window` and `current-worktree` ask something else
  entirely and `--force` does not reach them.
- **`is_ready()` is not "unclaimed".** It returns true for an already claimed
  story, so `dispatch` carries its own guard and `list` filters the
  active state out.
- **The claim is a hard precondition, not a trailing best-effort.** Storyhook's
  `state` *is* the claim marker, so `dispatch` claims via `--if-state` CAS
  before any side effect, and rolls the claim back if a later step fails.
- **Forced redispatch reuses a claim; it never rewrites or owns it.** `/story do
  <id> --force` is accepted only as the exception to the named story's
  already-claimed refusal. It skips the readiness lookup and redundant
  move for that one state, reports `reused_claim:true`/`claim_transitioned:false`,
  and leaves the pre-existing state untouched if a later dispatch step fails.
  Worktree, branch, tmux, provider-readiness and prompt-delivery checks remain
  unchanged; an existing worktree or branch returns `resume-available` rather
  than being treated as disposable.
- **Resume reconstructs; it never resets.** `dispatch <id> --resume` inventories
  the active claim, expected `worktree-<id>` branch, registered worktree, and
  named pane before any write. It reuses a valid dirty worktree, reattaches a
  branch-only survivor, creates only missing resources, and respawns an existing
  pane with `tmux respawn-pane -k`. The replacement charter names the previous
  agent and requires inspection before changes. Wrong branches, unregistered
  paths, protected identities, and the current pane refuse without deletion.
  Rollback removes only resources created by that attempt. Without permission,
  these same finds return typed `resume-available` plus a resource inventory;
  the interactive adapters ask once, while the dashboard always passes
  `--resume`. Engine lanes retain `resume:false`.
- **`complete` never forces anything by default**, and never touches tmux at
  all unless it is about to remove a worktree. `git worktree remove` runs
  without `--force` by default, and a branch is deleted only if merged into
  `origin/<default>` or local `<default>`; an un-comparable branch counts as
  *not* merged. **`--force` (SH-308) overrides `dirty`/`current` on the
  worktree only** — never `locked`, never an unmerged/protected branch. When a
  worktree is about to be removed (`removable` by default, or `dirty`/
  `current` under `--force`) and its dispatched tmux window is still open,
  `complete` closes that window FIRST — the opposite order from `reap`'s
  "kill last," because `complete` is answering an operator reclaiming
  *someone else's* worktree, not a session tearing down its own. The one
  window `complete` never closes, `--force` or not, is the one the caller is
  asking FROM — that stays `reap`'s job, which kills its own window last for
  the reason above.
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
  was running in. `reap` refuses outright unless the story is in the project's
  completion state (the first CLOSED state, or `STORY_DONE_STATE`) and the
  worktree/branch are both safe to discard. A different CLOSED state may mean
  abandoned work and is not accepted as completion — nothing partial, matching
  `complete`'s own "never forces anything by default" rule above but all-or-nothing
  rather than best-effort, since nobody is watching to read a partial
  result. The attended path is unchanged: teardown there still stays a later
  `/story complete <id>` from the main checkout.
- **`--auto` retains Plan mode but requires no person at the prompt.** Every
  autonomous tmux child receives `STORYHOOK_AUTO=<story-id>`. The packaged
  hook allows Claude's plan exit; dispatch arms provider-specific exact-pane
  watchers before prompt submission. Claude's sends Return only when the exact,
  selected Auto review option is visible in the original live child pane.
  Codex's sends Return to “Yes, implement this plan.”
  The hook denies either provider's question tool; Codex uses `--approve-for-me`
  for later tool approvals. Claude's
  post-plan default is `acceptEdits`, while Codex keeps workspace-write automatic
  review and bypasses only interactive trust for the packaged hook. A wholesale
  `STORY_LAUNCH_CMD` override is preserved and visibly reported as potentially
  weakening unattendedness.
- **Full Auto is an engine identity, not another spelling of `--auto`.** The
  helper accepts `--full-auto` only once, alongside `--auto` and a named story;
  `--next` and every incomplete composition are refused before a claim or
  resource exists. Each `tmux new-window` receives both markers explicitly:
  attended dispatch has `STORYHOOK_AUTO=` and `STORYHOOK_FULL_AUTO=`, ordinary
  Auto has `STORYHOOK_AUTO=<story-id>` and an empty Full Auto marker, and an
  engine lane has the inverse. No non-empty marker is written to tmux's session
  environment. Full Auto reuses the provider's built-in Auto command unless
  `STORY_FULL_AUTO_LAUNCH_CMD` is set; inherited `STORY_LAUNCH_CMD` is ignored
  and reported, so a daemon-wide expert override cannot silently weaken an
  engine lane. That lane is edit-capable only in its disposable worktree; the
  charter still forbids version/release/deploy work and permits merge only
  through the certified path.
- **Dispatch is provider-selected, not inferred from terminal prose.** Adapters pass
  `--agent=claude|codex`; an explicit dispatch flag overrides the active host and an
  omitted flag retains that adapter's host default. The helper also accepts
  `STORY_AGENT=claude|codex` for direct callers; `STORY_AGENT=claude-code` is a warned
  compatibility alias. Claude remains the default for callers that choose neither. Codex
  uses `codex --no-alt-screen`, `.codex/worktrees/`, screen readiness, a confirmed
  Shift+Tab transition into Plan mode, and Tab submission. Failure before submission keeps
  the existing rollback invariants intact. `doctor` reports and probes the selected
  provider rather than treating a Codex screen as Claude.
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
| `STORY_AGENT` | provider contract: `claude` (default) or `codex`; `claude-code` is a deprecated compatibility alias |
| `STORY_MODEL` / `STORY_EFFORT` / `STORY_SPEED` | dispatch's model/effort/speed selectors (ranked beneath an explicit `--model`/`--effort`/`--speed`, same as `STORY_AGENT` beneath `--agent`); each validated against the resolved provider's own catalog (`capabilities --agent=<p>`) before any claim or worktree side effect; combining any of the three with `STORY_LAUNCH_CMD`/`STORY_FULL_AUTO_LAUNCH_CMD` refuses, since those wholesale overrides have no seam for a selector |
| `STORY_DRY_RUN=1` | preview any side-effecting verb; changes nothing |
| `STORY_LAUNCH_CMD` | wholesale attended/ordinary-Auto launch override (must **not** include `-w`); ordinary autonomous results warn because it may weaken unattendedness; Full Auto reports and ignores it |
| `STORY_FULL_AUTO_LAUNCH_CMD` | engine-only wholesale launch override; when unset, Full Auto reuses the provider's built-in Auto command |
| `STORY_PROMPT` / `STORY_PROMPT_EXTRA` | the handoff prompt, and a clause appended to it |
| `STORY_AUTO_PROMPT` / `STORY_AUTO_PROMPT_SOLO` | the two `--auto` charters — council-available and no-council, respectively (same seam as `STORY_PROMPT`; either wins outright over the probe below); `<done-state>` renders as the project-specific completion state |
| `STORY_COUNCIL` | `auto` (default, probes for real)/`on`/`off` — which `--auto` charter `council_vote_available` picks |
| `STORY_DONE_STATE` | the completion state used by `complete`, autonomous prompt rendering, and `reap` |
| `STORY_TARGET_SESSION` | dispatch into a named session from outside tmux |
| `STORY_PROTECTED_BRANCHES` | extra globs `complete` must never delete |
| `STORY_REQUIRE_FRESH_BASE=1` | refuse to dispatch on a stale base instead of warning |

See `bin/story.sh`'s config block for the full list.

## Tests

```bash
bash plugins/story/tests/run-tests.sh          # all
bash plugins/story/tests/run-tests.sh complete # substring filter
```

They run against the **real** `story` binary (the repo builds it) and a real git
repo with a local bare origin under `/tmp` — deliberately not `$TMPDIR`, which
Spotlight indexes and which stalls file-intensive runs on macOS. Only `tmux` is
faked. `make test` builds the binary first and puts it on `PATH` for this suite,
so it always exercises the freshly built CLI rather than whatever is installed.
