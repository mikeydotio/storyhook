#!/usr/bin/env bash
set -euo pipefail

# Claude Code Stop hook for storyhook.
# Automatically generates a session handoff when the agent session ends.

read_plugin_config() {
  local key="$1" default="$2"
  local config=".storyhook/plugin-config.toml"
  if [[ ! -f "$config" ]]; then
    printf '%s' "$default"
    return
  fi
  local val
  val=$(grep "^${key}" "$config" 2>/dev/null | head -1 | sed 's/.*= *"//' | sed 's/".*//' || true)
  printf '%s' "${val:-$default}"
}

# Check if this is a storyhook project
if [[ ! -d ".storyhook" ]]; then
  printf '{}'
  exit 0
fi

# Check if plugin is enabled
enabled=$(read_plugin_config "enabled" "true")
if [[ "$enabled" == "false" ]]; then
  printf '{}'
  exit 0
fi

# Check if story binary is available
if ! command -v story &>/dev/null; then
  printf '{}'
  exit 0
fi

# Generate session handoff
handoff_output=$(story handoff --since 4h 2>/dev/null || echo "")

if [[ -n "$handoff_output" ]]; then
  # Escape for JSON output
  escaped=$(printf '%s' "$handoff_output" | python3 -c "
import sys, json
raw = sys.stdin.read()
print(json.dumps(raw), end='')
" 2>/dev/null || printf '"%s"' "$(printf '%s' "$handoff_output" | sed 's/\\/\\\\/g' | sed 's/"/\\"/g' | tr '\n' '\\n')")
  printf '{"systemMessage":%s}' "$escaped"
else
  printf '{}'
fi
