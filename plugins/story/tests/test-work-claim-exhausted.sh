#!/usr/bin/env bash
# SH-346: `story.sh work` with NO id, retrying against a pool that NEVER
# yields -- every claim attempt loses the race to the same rival (the fake
# answers `conflict` without touching real state, so `story next` keeps
# re-picking the identical story). This must terminate in a bounded
# claim-conflict refusal, never an unguarded overwrite and never an infinite
# loop.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Never claimable -- every attempt loses")
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')

real_story=$(command -v story)
out=$(
  cd "$repo" \
    && PATH="$TESTS_DIR/fakes/story-conflict:$PATH" \
      STORY_REAL_BIN="$real_story" \
      STORY_CONFLICT_ID="$id" STORY_CONFLICT_EXPECTED="$state" \
      bash "$SCRIPT" work 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "exhausted: ok:false, not a silent overwrite or hang"
assert_eq "$(jqf "$out" .reason)" "claim-conflict" "exhausted: reason is claim-conflict"
assert_contains "$(jqf "$out" .display)" "gave up after" "exhausted: display names the exhaustion, not a generic error"
assert_contains "$(jqf "$out" .display)" "claim attempts" "exhausted: display names WHAT was exhausted"

real_state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$real_state" "$state" "exhausted: the real story was never actually moved"

comment_count=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.comments|length')
assert_eq "$comment_count" "0" "exhausted: no start comment was posted"

finish
