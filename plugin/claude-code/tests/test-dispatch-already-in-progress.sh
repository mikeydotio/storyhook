#!/usr/bin/env bash
# Issue #40 gap this fork exists to close: storyhook's own is_ready()
# returns true even for a story already in-progress -- it only checks
# open/unblocked, not "hasn't been claimed yet". /story do must still
# refuse a redispatch, via an explicit guard checked BEFORE the ready-gate.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Already claimed")
(cd "$repo" && story move "$id" in-progress >/dev/null)

# Fixture sanity: storyhook's SH-236 fix (`is_claimable`, layered on top of
# `is_ready`) now excludes an in-progress story from `story list --ready`
# itself, so the already-in-progress guard below is no longer the only thing
# that would refuse this redispatch -- the ready-gate (deviation #2, step 6)
# would too, just with a more generic reason ("not in `story list --ready`")
# than this guard's specific one. The guard stays: its message names the
# likely cause and gives a concrete unstick command, which the ready-gate's
# fallback reason does not. This assertion pins the new ground truth so a
# future storyhook regression here is caught by *this* test rather than only
# by storyhook's own SH-236 regression tests.
still_ready=$(cd "$repo" && story list --ready --json 2>/dev/null | jq --arg id "$id" '([.stories[]?.story.id // empty] | index($id)) != null')
assert_eq "$still_ready" "false" "fixture sanity: an in-progress story is excluded from \`story list --ready\` (SH-236)"

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "already-in-progress: ok:false"
assert_contains "$(jqf "$out" .display)" "already in-progress" "already-in-progress: reason names it"

# No worktree created, no additional state churn.
[ -d "$repo/.claude/worktrees" ] && fail_test "already-in-progress: a worktree was created despite the refusal"
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "in-progress" "already-in-progress: state unchanged (still in-progress, not re-moved)"

finish
