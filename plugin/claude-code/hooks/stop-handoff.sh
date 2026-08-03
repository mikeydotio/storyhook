#!/usr/bin/env bash
set -euo pipefail

# Claude Code Stop hook for storyhook.
# Automatically generates a session handoff when the agent session ends.

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# Whether this directory is a storyhook project is storyhook's question, not a
# shell walk's: a fresh clone with no committed pointer file resolves by its
# registered origin. `story handoff` below is the gate — it refuses on stderr
# and prints nothing, which is already the "say nothing" answer.

# Check if a forge pipeline is active for this project. forge's own Stop
# hook (and freshen's) already fire on every pipeline step and forge writes
# its own handoff artifact, so generating a storyhook handoff here too is a
# redundant, duplicate injection on every forge step transition. forge owns
# handoff generation while it is managing this project's pipeline.
if [[ -d ".forge" ]]; then
  printf '{}'
  exit 0
fi

# Check if plugin is enabled
if ! hook_is_enabled; then
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
