#!/usr/bin/env bash
set -euo pipefail

# Claude Code PostToolUse hook for storyhook.
# After git commit/merge/push operations, syncs commit history with stories.

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

# Read stdin JSON (contains tool_input with the command that was run)
stdin_json=""
if ! [ -t 0 ]; then
  stdin_json=$(cat)
fi

if [[ -z "$stdin_json" ]]; then
  printf '{}'
  exit 0
fi

# Extract the command string from tool_input
command_str=$(printf '%s' "$stdin_json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
tool_input = data.get('tool_input', {})
if isinstance(tool_input, str):
    print(tool_input)
else:
    print(tool_input.get('command', ''))
" 2>/dev/null || echo "")

# Fast check: only act on git commit/merge/push commands
if [[ "$command_str" != *"git commit"* && "$command_str" != *"git merge"* && "$command_str" != *"git push"* ]]; then
  printf '{}'
  exit 0
fi

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

# Check if git hooks are already installed (avoid double sync)
if [[ -f ".git/hooks/post-commit" ]] && grep -q "# storyhook managed hook" ".git/hooks/post-commit" 2>/dev/null; then
  printf '{}'
  exit 0
fi

# Run sync
sync_output=$(story sync-git --since 1h --quiet 2>/dev/null || echo "")

if [[ -n "$sync_output" ]]; then
  # Escape for JSON
  escaped=$(printf '%s' "$sync_output" | sed 's/\\/\\\\/g' | sed 's/"/\\"/g' | tr '\n' ' ')
  printf '{"systemMessage":"[storyhook] Git sync: %s"}' "$escaped"
else
  printf '{}'
fi
