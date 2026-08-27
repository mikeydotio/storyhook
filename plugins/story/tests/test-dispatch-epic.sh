#!/usr/bin/env bash
# Epics are planning containers. Named dispatch must refuse them before --force
# or the ready gate can claim their computed state.
#
# SH-499: an epic is a story TYPED `epic`, never one that merely holds a
# `parent-of` edge. This fixture used to build its "structural epic" by giving
# an ordinary story a child and relying on that to confer epic-ness -- which is
# exactly the conflation SH-499 removed, so the fixture stated the defect. The
# epic is typed now, and the second half of this file asserts the other side:
# an ordinary story with a child is dispatchable, because it is work.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
epic=$(cd "$repo" && story new "Real epic" --type epic --json | jq -r '.story.story.id')
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

# --- the other side of SH-499 -------------------------------------------------
# A NORMAL story that happens to have a child is ordinary work, and dispatch
# must offer it. Before SH-499 this refused for the same reason the epic above
# does, so a bug that spawned a follow-up became permanently undispatchable.
parent=$(new_story "$repo" "A normal story with a sub-task")
subtask=$(new_story "$repo" "Its sub-task")
(cd "$repo" && story relate "$parent" parent-of "$subtask" >/dev/null)

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$parent" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "normal parent: dispatchable, because it is work"
case "$(jqf "$out" .display)" in
  *"is an epic"*) fail_test "normal parent: refused as an epic for having a child" ;;
esac

finish
