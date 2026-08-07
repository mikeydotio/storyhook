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

# Whether this repository wants the hook. Whether this directory *is* a
# storyhook project is not asked here: only storyhook can answer it — a fresh
# clone with no pointer file resolves by its registered origin — so the gate is
# `story commit-sync` itself, below. It refuses on stderr and prints nothing,
# which collapses to `{}` exactly as a disabled hook does.
if ! hook_is_enabled; then
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

# Run sync. --deadline 8: this hook has 10s (hooks.json) before Claude Code
# kills it; a cold daemon plus the store's own reply may legitimately take
# 150s (SH-182). 8s leaves 2s for this script itself, and gives up loudly
# into the `|| echo ""` fallback below rather than being killed mid-write.
sync_output=$(story --deadline 8 commit-sync --since 1h --quiet 2>/dev/null || echo "")

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
