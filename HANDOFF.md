# SH-582 Handoff

## Root cause

The long-lived daemon inherited the cwd of its initiating client. When a
worktree was removed, Claude and Codex provider children inherited that deleted
directory and plugin reinstall failed.

## Delivered

- `73ef69cb`: daemon entry changes to stable `Environment::home()` before any
  fallible initialization or request work.
- Two-phase Claude/Codex regression starts the real daemon in an ephemeral
  directory, deletes it, then proves reinstall succeeds through the same PID
  and provider cwd is canonical HOME.
- `764af159`: the machine-wide verifier tmux pane now uses explicit HOME cwd
  for session/window creation and banner/tail respawn.
- Verifier-window argv tests cover all long-lived pane forms with a spaced HOME
  path.

## RCA

- FULL artifacts: `.rca/claude-and-codex-plugin-reinstall/` (local, ignored).
- Confidence: HIGH. Live deleted daemon cwd plus deterministic reproduction and
  fail-pass-fail causal toggle.
- Classification: Assignment/Init Missing; SURGICAL.

## Focused verification

- Plugin cwd regression: 2/2.
- Daemon invoke: 7/7; daemon lifecycle: 28/28; lifecycle units: 47/47.
- Plugin install: 23/23; dispatch resolver: 12/12.
- Project command: 19/19; spawn inventory: 2/2.
- Verifier window: 8/8.
- Targeted Clippy with warnings denied, rustfmt, Bash syntax, and diff checks.

## Submission boundary

Push `fix/SH-582-daemon-cwd`, open and link one SH-582 PR, then move SH-582 to
`verifying` as the final action. Central verification owns the full suite,
merge, completion, and worktree cleanup. Do not version, release, deploy, or
merge from this linked worktree.
