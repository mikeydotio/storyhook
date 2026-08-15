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

# Skip this stand-in only when git will itself run a storyhook post-commit hook
# here. Which file that is is git's question to answer, not this script's: this
# used to test `.git/hooks/post-commit`, the `<root>/.git/hooks` assumption
# SH-313 and SH-314 removed from `src/`, which survived here in shell (SH-320).
# It was wrong in both directions. Open: a linked worktree's `.git` is a *file*,
# so the test could never resolve, and this synced on top of a hook that had
# already run. Closed, and worse: with `core.hooksPath` set, a marker left in
# `.git/hooks/post-commit` by an earlier install made this skip while git ran
# something else — and if that something else does not delegate, nothing synced
# at all, silently.
#
# `--git-path hooks` is `HookDirs::effective` asked from bash: the directory git
# will actually consult. Never `--git-common-dir/hooks` (`managed`) — a hook git
# has been told not to look at syncs nothing, so it is no reason to skip. Git
# answers relative to the cwd it is asked from, and this script never cds, so the
# answer is used as given; `HookDirs` joins onto `--show-toplevel` because it
# writes from the daemon against an envelope root, which is a different problem.
#
# `-x`, not `-f`: git runs only an *executable* hook, and stores that bit in the
# index — so a tracked hooks directory restored without mode 100755 holds a
# marker-bearing file git will skip. Asking git's own question costs at worst one
# redundant idempotent sync; asking a weaker one costs a silent miss.
#
# Both failure paths land on "sync", which is the safe direction: outside a
# repository `rev-parse` exits 128 printing nothing (the `||` is what keeps
# `set -e` from aborting the hook), and a `core.hooksPath` naming a non-directory
# — `/dev/null`, the documented way to switch hooks off — fails the `-x` test.
# ...and only when the command's verbs are ones that hook actually covers
# (SH-330). The guard above asks git *where* the hook is; it never asked what
# *happened*, though the answer was free in `$command_str`, parsed above.
#
# Stated positively: skip only when every git verb in this command is one a
# managed `post-commit` covers. `git commit` is; the others are not.
#
#   * `git push` runs no post-push hook — git has none, and neither does
#     `src/hooks.rs`'s HOOKS table. The guard skipped on a hook that cannot
#     have fired.
#   * `git merge` runs `post-merge`, not `post-commit` (measured, for both a
#     --no-ff merge and a fast-forward). Since SH-330 `post-merge` syncs too,
#     which makes the plugin's sync here redundant rather than necessary — but
#     redundant is the cheap direction and it is not a reason to skip: every
#     repository installed before that change keeps the old body until
#     `story hooks install` is re-run.
#
# Deliberately NOT extended to grep `post-merge` for the marker, which is the
# obvious next "optimisation" and is this same defect wearing a different
# hook's name: it would credit an installed file for work that file's version
# may not do. A redundant sync costs one warm round trip (~15ms, measured)
# against a silent miss.
#
# A compound command falls open: `git commit -m x && git merge side` ran both,
# only one is covered, so the command as a whole is not. Substring matching is
# the same imprecision the pre-filter above already accepts.
if [[ "$command_str" != *"git merge"* && "$command_str" != *"git push"* ]]; then
  git_hooks_dir=$(git rev-parse --git-path hooks 2>/dev/null) || git_hooks_dir=""
  if [[ -n "$git_hooks_dir" && -x "$git_hooks_dir/post-commit" ]] &&
    grep -q "# storyhook managed hook" "$git_hooks_dir/post-commit" 2>/dev/null; then
    printf '{}'
    exit 0
  fi
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
