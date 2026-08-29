# Codex dispatch adapter

Load this adapter only when the active agent host is Codex. The shared `story` skill has
already resolved the installed plugin root and its absolute `<story-helper>` path.

## Dispatch (`do <id> [--auto] [--force] [--agent=claude|codex]`)

Run `bash "<story-helper>" dispatch <id> --agent=<agent>`, adding `--auto` and/or `--force`
only when the user requested them. Use the user's explicit agent when present; otherwise use
`--agent=codex`. `--force` reuses a named story's existing claim without
another state transition; it does not override any worktree, branch, tmux, or provider
safety gate.

- `ok:false`: show `display` and stop. The helper refuses before prompt delivery when the
  story, worktree, Codex process, readiness screen, or Plan-mode footer is unsafe.
- `ok:true`: show `display` verbatim. Surface `warning` and a fenced `pane_tail` when present.

The helper owns the compare-and-swap claim, fresh base, `.codex/worktrees/<id>` worktree,
tmux window, `codex --no-alt-screen` launch, screen readiness, Shift+Tab transition into Plan
mode, bracketed paste, and Tab submission. Do not repeat those side effects.

Codex has no stable machine-readable skill inventory. In `--auto`, council discovery
therefore defaults to the safe solo charter; `STORY_COUNCIL=on` is the explicit opt-in.

## Capture and doctor

- `capture <id>`: run `STORY_AGENT=codex bash "<story-helper>" capture <id>` and show `display`.
- `doctor`: run `STORY_AGENT=codex bash "<story-helper>" doctor` and show `display`. It reports
  the selected provider and independently confirms readiness, Plan mode, bracketed paste,
  and Storyhook project integrity.

## Release (`unclaim <id>`, `reset <id>`)

Reached as `$story unclaim <id>` and `$story reset <id>`. Run
`bash "<story-helper>" unclaim <id>` or `bash "<story-helper>" reset <id>`, adding `--force`
(reset only) and `--comment <text>` / `--no-comment` only when the user asked for them. Show
`display` and stop. Both close the story's tmux window, which is why they are documented per
host rather than only in the shared router.

- `unclaim` leaves the `.codex/worktrees/<id>` worktree and its branch exactly as found;
  `reset` deletes both.
- From the story's own window, `unclaim` still releases the claim and reports that it left the
  window open, while `reset` refuses with `self-window` and `--force` does not override it.

## Hooks and trust

Codex discovers this installed plugin's `hooks/hooks.json`. The same SessionStart,
PreToolUse, PostToolUse(Bash), and Stop hook protocol is shared with Claude Code. A locally
installed, non-managed plugin may require explicit trust/review in Codex before its hooks run.

The PreToolUse entries are Full Auto's (`hooks/full-auto.sh`, SH-460), and they are inert in an
ordinary session: with `STORYHOOK_FULL_AUTO` unset the hook emits no decision. Inside an engine
lane it approves the plan exit and refuses the question-asking tool, handing the model an
instruction to decide or convene a council instead of waiting for a person who is not there.
Codex's arm was measured live rather than assumed (SH-459, CLI 0.149.0): a matcher named
`request_user_input` runs before the question UI, and `permissionDecisionReason` is returned to
the model as the blocking reason. On both hosts a PreToolUse hook fails OPEN at its timeout, so
a lane whose denial times out asks anyway and stalls — caught by the engine's stall ceiling and
quarantined, never silent.
