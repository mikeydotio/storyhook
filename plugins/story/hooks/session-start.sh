#!/usr/bin/env bash
set -euo pipefail

# Agent SessionStart hook for storyhook (Codex and Claude Code).
# Delegates to `story session-start` which handles all logic internally.

# Read the provider's stdin JSON (provides session context with cwd).
stdin_json=""
if ! [ -t 0 ]; then
  stdin_json=$(cat)
fi

# Extract cwd from stdin JSON and cd to it.
if [[ -n "$stdin_json" ]]; then
  cwd=$(printf '%s' "$stdin_json" | sed -n 's/.*"cwd" *: *"\([^"]*\)".*/\1/p')
  if [[ -n "$cwd" && -d "$cwd" ]]; then
    cd "$cwd"
  fi
fi

# Delegate to story session-start; emit only a JSON envelope, never leaked text.
#
# The CLI writes usage/error output to stdout (not stderr), so a stale `story`
# binary that predates the `session-start` subcommand would otherwise dump
# "error: unknown command `session-start`. Run `story --help` for usage." into
# the session. Capture stdout and pass it through only when it is JSON (starts
# with `{`); a non-zero exit blanks it and anything else collapses to `{}`.
#
# --deadline: this hook has hooks.json's own outer `timeout` before the
# provider kills it; a cold daemon start plus the store's own reply may
# legitimately take 150s (SH-182). An ordinary human-launched session gives up
# after 3s (hooks.json allows 25, leaving 22s of slack this branch does not
# use) and turns that into a recoverable {} rather than nothing — nobody
# should wait on their own `claude` launch for a slow daemon.
#
# A DISPATCHED pane is different (SH-544): `story.sh dispatch` sets
# STORYHOOK_DISPATCH=1 via `tmux new-window`/`respawn-pane -e` on every
# dispatch it opens, and its own readiness gate (READY_ATTEMPTS *
# READY_DELAY, `tests/dispatch_ready_budget.rs` pins it against
# SPAWN_LOCK_DEADLINE) is already prepared to poll for tens of seconds — a
# dispatched session is watched by a poll, not by an impatient human, so it
# can afford the same order of patience this project already grants an
# ordinary `story` command against daemon contention. hooks.json's own
# ceiling for THIS hook (25s) was raised to give this branch room; the
# ordinary case's own 3s budget is untouched, so a human launch's own latency
# is unaffected either way.
#
# Two literal `story --deadline <n> session-start` call sites, not one
# variable-driven call: `tests/hook_budgets.rs`'s `deadline_flag` is a plain
# text scan for exactly that literal shape (deliberately, to cost no wall
# clock — see that file's own header), and a computed `--deadline "$var"`
# would read to it as "no deadline declared" and fail the build.
#
# The original hook JSON (already captured above, cwd extracted from it) is
# re-piped into `story`'s OWN stdin rather than consumed only here: its
# `session_id` field is what SH-231's dispatch sentinel is named with, and
# `story`'s CLI already has a generic stdin-passthrough seam (`reads_stdin`,
# used by `import`/`decompose`) rather than needing a bespoke flag for one
# more field. Piping an empty string when nothing arrived is a no-op — an
# immediately-closed stdin reads as "".
if command -v story &>/dev/null; then
  if [ -n "${STORYHOOK_DISPATCH:-}" ]; then out=$(printf '%s' "$stdin_json" | story --deadline 20 session-start 2>/dev/null) || out=""; else out=$(printf '%s' "$stdin_json" | story --deadline 3 session-start 2>/dev/null) || out=""; fi
  case "$out" in "{"*) printf '%s' "$out" ;; *) printf '{}' ;; esac
else
  printf '{}'
fi
