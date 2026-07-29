#!/usr/bin/env bash
set -euo pipefail

# Claude Code PostToolUse hook for storyhook.
# After git commit/merge/push operations, syncs commit history with stories.

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# Read stdin JSON (contains tool_input with the command that was run)
stdin_json=""
if ! [ -t 0 ]; then
  stdin_json=$(cat)
fi

if [[ -z "$stdin_json" ]]; then
  printf '{}'
  exit 0
fi

# Cheap pre-filter on the raw JSON text, before paying a python3 interpreter
# startup for every single Bash tool call this hook fires on. JSON encoders
# only escape structural characters (quotes, backslashes, control chars) —
# plain ASCII words like "git commit" survive intact in the raw payload, so
# a literal substring check here has no false negatives for the patterns we
# care about. This hook only needs to actually parse tool_input (below) for
# the rare call that plausibly touches git.
case "$stdin_json" in
  *"git commit"*|*"git merge"*|*"git push"*) ;;
  *)
    printf '{}'
    exit 0
    ;;
esac

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

# Confirm the match wasn't a false positive from the cheap pre-filter (e.g.
# the literal text appearing somewhere other than the actual command, or
# JSON parsing failing) before treating this as a real git commit/merge/push.
if [[ "$command_str" != *"git commit"* && "$command_str" != *"git merge"* && "$command_str" != *"git push"* ]]; then
  printf '{}'
  exit 0
fi

# Check if this is a storyhook project, and whether it wants the hook
if ! storyhook_pointer >/dev/null || ! hook_is_enabled; then
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
  # Build the JSON with python3's json.dumps rather than manual sed escaping,
  # which only handled backslash/quote/newline and would emit invalid JSON
  # on other control characters (e.g. a tab in $sync_output).
  printf '%s' "$sync_output" | python3 -c "
import sys, json
msg = '[storyhook] Git sync: ' + sys.stdin.read().strip()
print(json.dumps({'systemMessage': msg}))
"
else
  printf '{}'
fi
