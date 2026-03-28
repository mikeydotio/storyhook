#!/usr/bin/env bash
set -euo pipefail

# Claude Code SessionStart hook for storyhook.
# Reads project state and emits a concise system message at session start.

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

# Read stdin JSON (Claude Code provides session context)
stdin_json=""
if ! [ -t 0 ]; then
  stdin_json=$(cat)
fi

# Try to extract cwd from stdin JSON
if [[ -n "$stdin_json" ]]; then
  cwd=$(printf '%s' "$stdin_json" | python3 -c "import sys,json; print(json.load(sys.stdin).get('cwd',''))" 2>/dev/null || true)
  if [[ -n "$cwd" && -d "$cwd" ]]; then
    cd "$cwd"
  fi
fi

# Check if this is a storyhook project
if [[ ! -d ".storyhook" ]]; then
  printf '{}'
  exit 0
fi

# Check if story binary is available
if ! command -v story &>/dev/null; then
  printf '{"systemMessage":"[storyhook] The `story` CLI is not installed. Run `/storyhook:setup` to install and configure storyhook."}'
  exit 0
fi

# Check if plugin is enabled
enabled=$(read_plugin_config "enabled" "true")
if [[ "$enabled" == "false" ]]; then
  printf '{}'
  exit 0
fi

# Gather project state
summary_json=$(story summary --json 2>/dev/null || echo "")
next_json=$(story next --count 1 --json 2>/dev/null || echo "")

if [[ -z "$summary_json" ]]; then
  printf '{}'
  exit 0
fi

# Extract key stats from summary JSON
open=$(printf '%s' "$summary_json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
counts = data.get('state_counts', data.get('states', {}))
total = 0
for k, v in counts.items():
    total += v
print(total)
" 2>/dev/null || echo "?")

ready=$(printf '%s' "$summary_json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
print(data.get('ready_count', data.get('ready', '?')))
" 2>/dev/null || echo "?")

# Extract next story info
next_info=""
if [[ -n "$next_json" ]]; then
  next_info=$(printf '%s' "$next_json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
stories = data if isinstance(data, list) else data.get('stories', [data])
if stories:
    s = stories[0]
    sid = s.get('id', '?')
    title = s.get('title', '?')
    pri = s.get('priority', '')
    parts = [f\"{sid} '{title}'\"]
    if pri and pri != 'none':
        parts.append(f'({pri} priority)')
    print(' '.join(parts))
else:
    print('')
" 2>/dev/null || echo "")
fi

# Build message
msg="[storyhook] ${open} open stories, ${ready} ready."
if [[ -n "$next_info" ]]; then
  msg="${msg} Next: ${next_info}"
fi

printf '{"systemMessage":"%s"}' "$(printf '%s' "$msg" | sed 's/"/\\"/g')"
