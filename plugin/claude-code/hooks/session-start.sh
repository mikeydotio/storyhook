#!/usr/bin/env bash
set -euo pipefail

# Claude Code SessionStart hook for storyhook.
# Delegates to `story session-start` which handles all logic internally.

# Read stdin JSON from Claude Code (provides session context with cwd).
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
if command -v story &>/dev/null; then
  out=$(story session-start 2>/dev/null) || out=""
  case "$out" in "{"*) printf '%s' "$out" ;; *) printf '{}' ;; esac
else
  printf '{}'
fi
