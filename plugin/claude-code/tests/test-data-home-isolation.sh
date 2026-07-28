#!/usr/bin/env bash
# The suite must never write into the developer's real storyhook data.
#
# This is not a hypothetical. storyhook is moving from per-project
# `.storyhook/` directories to a single global store under the XDG data home,
# and the fixtures here create dozens of projects and stories per run. Without
# isolation, the first `make test` after that flip lands would pour them into
# the tracker this project uses to track itself -- silently, and with no undo.
#
# So: the isolation is asserted, not assumed, and asserted by running a real
# `story` command and then looking at what the real data home actually
# contains.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# --- the variables themselves ---

for var in HOME XDG_DATA_HOME XDG_CONFIG_HOME XDG_STATE_HOME STORYHOOK_DATA_DIR; do
  value="${!var:-}"
  if [ -z "$value" ]; then
    fail_test "\$$var must be set before any fixture runs"
    continue
  fi
  case "$value" in
  /tmp/* | /private/tmp/*) : ;;
  *) fail_test "\$$var must live under /tmp, got [$value]" ;;
  esac
done

if [ -n "${STORYHOOK_REAL_HOME:-}" ]; then
  [ "$HOME" = "$STORYHOOK_REAL_HOME" ] &&
    fail_test "\$HOME is still the developer's real home ($HOME)"
else
  fail_test "\$STORYHOOK_REAL_HOME must be preserved so this test can check the real data home"
fi

# --- and what a real `story` run does to the real data home ---
#
# Snapshot before/after rather than asserting emptiness: the developer's real
# store legitimately has content in it, and what matters is that this run adds
# nothing to it.
# Falls back to $HOME so this still checks something real when
# STORYHOOK_REAL_HOME is missing -- in that case $HOME *is* the developer's
# home, which is precisely the situation worth catching.
real_data_home="${STORYHOOK_REAL_HOME:-$HOME}/.local/share/storyhook"
snapshot() {
  if [ -d "$1" ]; then find "$1" | sort; else echo "<absent>"; fi
}
before="$(snapshot "$real_data_home")"

repo="$(mk_story_repo)"
id="$(new_story "$repo" "isolation probe")"
assert_contains "$id" "TST-" "fixture sanity: the story CLI ran and minted an id"

after="$(snapshot "$real_data_home")"
assert_eq "$after" "$before" "a test run must not touch $real_data_home"

# The fixture's own storyhook state must be somewhere isolated, never in the
# real home -- the same check from the other direction.
project_state="$repo/.storyhook"
[ -d "$project_state" ] || fail_test "fixture sanity: $project_state should exist"
case "$repo" in
/tmp/* | /private/tmp/*) : ;;
*) fail_test "fixtures must live under /tmp, got [$repo]" ;;
esac

finish
