# Complete a story — close it and reclaim its worktree

Loaded on demand by the `story` router. Two phases: a read-only preview, then a
confirmed execution. **Route and render — never call `story`, `git`, or `tmux`
yourself.**

This is the other half of provider dispatch. Without it, every dispatched story leaves
a worktree and a branch behind forever.

The loading skill has already resolved `<story-helper>` from its installed `SKILL.md`
location. Substitute and shell-quote that absolute path in every command below; never
resolve packaged files from the user's current working directory.

## 1. Plan (read-only)

```
bash "<story-helper>" complete plan <id>
```

`ok:false` → show `display`, stop.

The result reports, for each target, what would happen and why:

- `plan.close` — `null` if the story is already closed, else the state it would
  move into (resolved from the project's own state catalog, not hard-coded).
- `plan.worktree.status` — `removable` | `dirty` | `locked` | `current` |
  `missing`.
- `plan.branch.status` — `deletable` | `unmerged` | `protected` | `missing`.
- `plan.window.status` — `open` | `self` | `none`: whether the dispatched tmux
  window for this story is still alive. `self` means the window you are asking
  completion FROM — see below.
- `actions_count` — how many of the four are actually actionable. An `open`
  window only counts when the worktree is `removable` by default (no
  `--force` yet) — see step 3's `--force` option.

## 2. Early exit

If `actions_count` is `0`, show `display` and **stop without asking**. There is
nothing to confirm.

## 3. Confirm

Ask exactly one concise confirmation question, using the host's structured question
mechanism when available and showing the plan's `display` above it. Options:

| Option | Runs |
|---|---|
| **Proceed** | `complete execute <id>` — close the story *and* clean up |
| **Close only** | `complete execute <id> --no-clean` — close, keep the worktree |
| **Cancel** | nothing |

(`--no-close` also exists — clean up but leave the story open — if the user
explicitly asks for that instead.)

If the plan reports `plan.worktree.status` as `dirty` or `current` (never
`locked` — that one has no override), add a fourth option:

| Option | Runs |
|---|---|
| **Force** | `complete execute <id> --force` — close the story, close the window if one is open, and remove the worktree **discarding what `plan` said would be preserved** |

Name plainly what `--force` discards before offering it — uncommitted files for
`dirty`, "the directory your shell is standing in" for `current`.

## 4. Execute

Run the chosen command and show its `display`. Surface any `failed` array
prominently; `skipped` entries are normal and explain what was deliberately
preserved.

## What it will never do

Say so plainly if the user asks why something survived — these are guard rails,
not failures:

- **An unmerged branch is never deleted, even with `--force`.** Merged-ness is
  judged against the union of `origin/<default>` and local `<default>`, with
  `origin/<default>` freshened first (a worktree-driven repo's local `main`
  often lags). A branch that can't be compared to either is treated as **not**
  merged. `--force` only ever widens what happens to the *worktree*.
- **A dirty worktree is never removed by default** — `git worktree remove` runs
  without `--force`, so git's own refusal stands. `--force` overrides this one,
  deliberately, discarding uncommitted changes; the confirmation step names
  that before offering it.
- **A locked worktree is never removed and never unlocked, `--force` or not.**
  A provider may lock worktrees it creates itself; reclaiming those is not
  this workflow's responsibility.
- **The current worktree is never removed by default** — run `complete` from
  the main checkout if you're standing inside the one you want reclaimed.
  `--force` overrides this one too, same as `dirty`.
- **`main`/`master`, the default branch, and anything in
  `STORY_PROTECTED_BRANCHES` are never touched.**
- **The window you are asking completion FROM (`self`) is never
  closed**, whether or not `--force` is given — closing it would destroy the
  session before it could show you the result. `reap` exists for exactly that
  self-directed case, and closes its own window LAST, once destructive git
  work is already done.
- **A dispatched window is closed only when its worktree is actually about to
  be removed** — never on a `dirty`/`current` worktree that `--force` didn't
  override, since nothing is being reclaimed out from under it. When it does
  close, it closes **before** the worktree, never after — so nothing keeps
  running inside a directory mid-deletion.

## Ordering

The story is closed **first**, and best-effort: if the close fails, cleanup still
runs and the failure is reported as a note rather than flipping `ok` to false.

Within cleanup, a dispatched window that is about to lose its worktree is
closed **before** the worktree is removed — the opposite of `reap`, which
closes its own window **last**. The two verbs answer different questions:
`reap` is a session tearing down its own workspace once nothing further needs
to run in it, so the window survives until everything else is done; `complete`
is an operator reclaiming a worktree that may still have a *bystander* window
sitting in it, so that window has to go first or it would be left running
inside a directory mid-deletion.
