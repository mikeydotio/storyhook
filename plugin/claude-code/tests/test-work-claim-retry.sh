#!/usr/bin/env bash
# SH-346: `story.sh work` with NO id races `story next`'s pick against a
# rival that claims it first. Since the caller asked for ANY ready story (not
# a specific one), a lost race here must NOT be a hard refusal like the
# explicit-id path -- it must re-pick and land a DIFFERENT ready story,
# leaving the rival's claim alone and posting exactly one start comment (on
# the story this session actually landed).
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id_a=$(new_story "$repo" "Wins the race (a rival claims it)")
id_b=$(new_story "$repo" "The retry should land here instead")
(cd "$repo" && story prioritize "$id_a" critical >/dev/null 2>&1)
(cd "$repo" && story prioritize "$id_b" high >/dev/null 2>&1)

# Confirm `story next` really picks id_a first, so the fake's rival-claim
# target is the story the SUT will actually try first -- not an assumption.
picked_first=$(cd "$repo" && story next --json | jq -r '.story.story.id')
assert_eq "$picked_first" "$id_a" "retry setup: story next picks the higher-priority story first"
state_a=$(cd "$repo" && story show "$id_a" --json | jq -r '.story.story.state')

real_story=$(command -v story)
out=$(
  cd "$repo" \
    && PATH="$TESTS_DIR/fakes/story-conflict:$PATH" \
      STORY_REAL_BIN="$real_story" \
      STORY_CONFLICT_ID="$id_a" STORY_CONFLICT_EXPECTED="$state_a" STORY_CONFLICT_RIVAL=1 \
      bash "$SCRIPT" work 2>&1
)
assert_eq "$(jqf "$out" .ok)" "true" "retry: ok"
assert_eq "$(jqf "$out" .picked)" "true" "retry: picked something"
assert_eq "$(jqf "$out" .id)" "$id_b" "retry: landed the OTHER ready story after losing the first race"
assert_eq "$(jqf "$out" .moved)" "true" "retry: the retry's own claim succeeded"

real_state_a=$(cd "$repo" && story show "$id_a" --json | jq -r '.story.story.state')
assert_eq "$real_state_a" "in-progress" "retry: the rival's claim on the first story is real, not a canned illusion"
real_state_b=$(cd "$repo" && story show "$id_b" --json | jq -r '.story.story.state')
assert_eq "$real_state_b" "in-progress" "retry: the second story really got claimed by this session"

comments_a=$(cd "$repo" && story show "$id_a" --json | jq -r '.story.story.comments|length')
assert_eq "$comments_a" "0" "retry: the rival's story got no start comment from this session"
comments_b=$(cd "$repo" && story show "$id_b" --json | jq -r '.story.story.comments|length')
assert_eq "$comments_b" "1" "retry: exactly one start comment, on the story actually claimed"

finish
