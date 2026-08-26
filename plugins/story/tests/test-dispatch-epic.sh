#!/usr/bin/env bash
# Structural epics are planning containers. Named dispatch must refuse them
# before --force or the ready gate can claim their computed state.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
epic=$(new_story "$repo" "Structural epic")
child=$(new_story "$repo" "Actionable child")
(cd "$repo" && story relate "$epic" parent-of "$child" >/dev/null)

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$epic" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "epic: ok:false"
assert_contains "$(jqf "$out" .display)" "is an epic" "epic: refusal names the structure"
assert_contains "$(jqf "$out" .display)" "dispatch a ready child" "epic: refusal gives the next action"

forced=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$epic" --force 2>&1)
assert_eq "$(jqf "$forced" .ok)" "false" "forced epic: ok:false"
assert_contains "$(jqf "$forced" .display)" "computed from its children" "forced epic: state remains computed"

state=$(cd "$repo" && story show "$epic" --json | jq -r '.story.story.state')
assert_eq "$state" "todo" "epic: refused dispatch leaves effective state untouched"

finish
