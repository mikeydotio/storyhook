# `/story complete <id>` — close the story, reclaim the worktree

Loaded on demand by the `story` router. Two phases: a read-only preview, then a
confirmed execution. **Route and render — never call `story`, `git`, or `tmux`
yourself.**

This is the other half of `/story do`. Without it, every dispatched story leaves
a worktree and a branch behind forever.

## 1. Plan (read-only)

```
bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh complete plan <id>
```

`ok:false` → show `display`, stop.

The result reports, for each target, what would happen and why:

- `plan.close` — `null` if the story is already closed, else the state it would
  move into (resolved from the project's own state catalog, not hard-coded).
- `plan.worktree.status` — `removable` | `dirty` | `locked` | `current` |
  `missing`.
- `plan.branch.status` — `deletable` | `unmerged` | `protected` | `missing`.
- `actions_count` — how many of the three are actually actionable.

## 2. Early exit

If `actions_count` is `0`, show `display` and **stop without asking**. There is
nothing to confirm.

## 3. Confirm

**Exactly one `AskUserQuestion`**, `header: "Complete <id>"`, with the plan's
`display` shown fenced above it. Options:

| Option | Runs |
|---|---|
| **Proceed** | `complete execute <id>` — close the story *and* clean up |
| **Close only** | `complete execute <id> --no-clean` — close, keep the worktree |
| **Cancel** | nothing |

(`--no-close` also exists — clean up but leave the story open — if the user
explicitly asks for that instead.)

## 4. Execute

Run the chosen command and show its `display`. Surface any `failed` array
prominently; `skipped` entries are normal and explain what was deliberately
preserved.

## What it will never do

Say so plainly if the user asks why something survived — these are guard rails,
not failures:

- **An unmerged branch is never deleted.** Merged-ness is judged against the
  union of `origin/<default>` and local `<default>`, with `origin/<default>`
  freshened first (a worktree-driven repo's local `main` often lags). A branch
  that can't be compared to either is treated as **not** merged.
- **A dirty worktree is never removed** — `git worktree remove` runs without
  `--force`, so git's own refusal stands.
- **A locked worktree is never removed and never unlocked.** Claude Code locks
  the worktrees it creates itself; reclaiming those isn't this verb's business.
- **The current worktree is never removed** — run `complete` from the main
  checkout if you're standing inside the one you want reclaimed.
- **`main`/`master`, the default branch, and anything in
  `STORY_PROTECTED_BRANCHES` are never touched.**

## Ordering

The story is closed **first**, and best-effort: if the close fails, cleanup still
runs and the failure is reported as a note rather than flipping `ok` to false.
