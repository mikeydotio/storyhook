#!/usr/bin/env bash
# SH-346: `story.sh work <id>` claims through the same `--if-state` CAS guard
# `dispatch` already uses (test-dispatch-claim-conflict.sh's sibling for
# `work`). A rival winning the race between story.sh's own state read and its
# claim attempt must refuse with reason:"claim-conflict", leave the real
# story untouched, and post NO start comment -- never silently overwrite the
# rival's claim.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Raced by an explicit claim")
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')

real_story=$(command -v story)
out=$(
  cd "$repo" \
    && PATH="$TESTS_DIR/fakes/story-conflict:$PATH" \
      STORY_REAL_BIN="$real_story" \
      STORY_CONFLICT_ID="$id" STORY_CONFLICT_EXPECTED="$state" \
      bash "$SCRIPT" work "$id" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "explicit id conflict: ok:false"
assert_eq "$(jqf "$out" .reason)" "claim-conflict" "explicit id conflict: reason is claim-conflict"
assert_contains "$(jqf "$out" .display)" "another session likely won the race" "explicit id conflict: display names the cause"

real_state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$real_state" "$state" "explicit id conflict: real story state is untouched by the fake's canned response"

comment_count=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.comments|length')
assert_eq "$comment_count" "0" "explicit id conflict: no start comment was posted on a lost claim"

finish
