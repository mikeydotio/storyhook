# Codex dispatch adapter

Load this adapter only when the active agent host is Codex. The shared `story` skill has
already resolved the installed plugin root and its absolute `<story-helper>` path.

## Dispatch (`do <id> [--auto]`)

Run `STORY_AGENT=codex bash "<story-helper>" dispatch <id>`, adding `--auto` only when the user
requested it.

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

## Hooks and trust

Codex discovers this installed plugin's `hooks/hooks.json`. The same SessionStart,
PostToolUse(Bash), and Stop hook protocol is shared with Claude Code. A locally installed,
non-managed plugin may require explicit trust/review in Codex before its hooks run.
