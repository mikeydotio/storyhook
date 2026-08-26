# Claude Code dispatch adapter

Load this adapter only when the active agent host is Claude Code. The shared `story` skill
has already resolved the installed plugin root and its absolute `<story-helper>` path.

Claude exposes the main skill as the `/story` slash command. Infer the operation and arguments
from that invocation; do not rely on a magic environment variable.

## Dispatch (`do <id> [--auto] [--force] [--agent=claude|codex]`)

Run `bash "<story-helper>" dispatch <id> --agent=<agent>`, adding `--auto` and/or `--force`
only when the user requested them. Use the user's explicit agent when present; otherwise use
`--agent=claude`. Never pass the compatibility-only `claude-code` token through this
interface. `--force` reuses a named story's existing claim without another
state transition; it does not override any worktree, branch, tmux, or provider safety gate.

- `ok:false`: show `display` and stop. Common causes include a missing or closed story, an
  existing claim without `--force`, an unready state, or extra arguments.
- `ok:true`: show `display` verbatim. Surface `warning` and a fenced `pane_tail` when present.

The helper owns ready-state validation, the compare-and-swap claim, the fresh
`origin/<default>` base, the `.claude/worktrees/<id>` worktree, the tmux window, Claude's
plan-mode launch and readiness checks, and prompt submission. Do not repeat those side effects.

`--auto` selects the autonomous handoff charter. The helper's `STORY_AUTO_PROMPT` and
`STORY_AUTO_PROMPT_SOLO` templates govern decision recording, tests, merge behavior, story
closure, and the final `story.sh reap <id>` cleanup. `STORY_COUNCIL` controls council probing.
Do not rewrite those templates in prose.

## Capture (`capture <id>`)

Run `STORY_AGENT=claude bash "<story-helper>" capture <id>`, show `display`, and stop.
This is read-only.

## Doctor (`doctor`)

Run `STORY_AGENT=claude bash "<story-helper>" doctor` and show `display`. This checks
project integrity plus the Claude launch, readiness, and prompt-submission contract.

## Release (`unclaim <id>`, `reset <id>`)

Reached as `/story unclaim <id>` and `/story reset <id>`. Run
`bash "<story-helper>" unclaim <id>` or `bash "<story-helper>" reset <id>`, adding `--force`
(reset only) and `--comment <text>` / `--no-comment` only when the user asked for them. Show
`display` and stop. These are here rather than in the shared router because both close the
story's tmux window, which is terminal behavior this host owns.

- `unclaim` leaves the worktree and branch exactly as it found them; `reset` deletes both.
- Neither is safe to run from the story's own window in the same way. `unclaim` does the
  release and reports that it left that window open; `reset` refuses with `self-window`, and
  `--force` does not override it. Tell the user to run `reset` from another window.
- Claude-created worktrees may be locked. `reset` refuses a locked worktree by default and
  never unlocks one; `--force` removes it lock and all, which is an explicit choice the user
  has to make.

## Completion and setup notes

- Claude-created worktrees may be locked; the shared completion workflow preserves locked
  worktrees and never unlocks them.
- The legacy Claude instruction integration is available as
  `bash "<story-helper>" scaffold-claude-md`. Use it only when the user explicitly asks for a
  `CLAUDE.md` Storyhook block; ordinary shared setup uses Storyhook's canonical `AGENTS.md`
  scaffold.
- `do`, `capture`, and `doctor` require tmux unless `STORY_TARGET_SESSION` supplies a target
  for a non-interactive caller.
