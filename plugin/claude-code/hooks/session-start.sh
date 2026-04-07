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

# Delegate to story session-start; fall back to {} on any failure.
if command -v story &>/dev/null; then
  story session-start 2>/dev/null || printf '{}'
else
  printf '{}'
fi
