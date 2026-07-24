#!/usr/bin/env bash
# Issue #40 gap this fork exists to close: storyhook's own is_ready()
# returns true even for a story already in-progress -- it only checks
# open/unblocked, not "hasn't been claimed yet". /story do must still
# refuse a redispatch, via an explicit guard checked BEFORE the ready-gate.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Already claimed")
(cd "$repo" && story move "$id" in-progress >/dev/null)

# Fixture sanity: confirm the CLI's own readiness view still considers this
# story ready -- the exact gap the already-in-progress guard exists to
# close. If this ever stops being true (storyhook's is_ready() changes to
# also exclude in-progress stories), this guard becomes redundant with the
# ready-gate rather than load-bearing -- worth knowing, not a silent drift.
still_ready=$(cd "$repo" && story list --ready --json 2>/dev/null | jq --arg id "$id" '([.stories[]?.story.id // empty] | index($id)) != null')
assert_eq "$still_ready" "true" "fixture sanity: an in-progress story remains in \`story list --ready\`"

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "already-in-progress: ok:false"
assert_contains "$(jqf "$out" .display)" "already in-progress" "already-in-progress: reason names it"

# No worktree created, no additional state churn.
[ -d "$repo/.claude/worktrees" ] && fail_test "already-in-progress: a worktree was created despite the refusal"
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "in-progress" "already-in-progress: state unchanged (still in-progress, not re-moved)"

finish
