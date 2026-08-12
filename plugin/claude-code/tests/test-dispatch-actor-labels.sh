#!/usr/bin/env bash
# story.sh's writes declare WHICH code path made them (SH-246).
#
# storyhook records the command it dispatched against every event, but both the
# dispatch claim and its rollback are `story move` — so the trail cannot tell
# them apart on the command alone. That is exactly the ambiguity that made
# SH-239's reversion take a store dump and three source files to explain: a
# story went to `in-progress` and back to `todo` 106 seconds later, and nothing
# recorded that dispatch had released a claim it could not fulfil.
#
# This asserts the labels survive, end to end, through the real CLI and the real
# store — `story log` is read back rather than the script's own output, because
# what matters is what got *written*, not what story.sh believed it sent.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "A story to claim")

# A dispatch that claims and then fails after the claim, so both halves run in
# one go. The readiness gate cannot pass against the fake tmux, so dispatch
# rolls its own claim back — which is the pair this test is about.
(
  cd "$repo" \
    && PATH="$TESTS_DIR/fakes/tmux:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      bash "$SCRIPT" dispatch "$id" >/dev/null 2>&1
) || true

log=$(cd "$repo" && story log "$id" --json)

claim_actor=$(printf '%s' "$log" | jq -r '[.log[] | select(.actor == "story.sh:dispatch")] | length')
assert_eq "$claim_actor" "1" "the claim is labelled story.sh:dispatch"

# The rollback only happens if the claim was made and dispatch then failed. If
# a future change makes dispatch succeed against the fake, this assertion is the
# thing that should be revisited — not deleted.
rollback_actor=$(printf '%s' "$log" | jq -r '[.log[] | select(.actor == "story.sh:dispatch-rollback")] | length')
assert_eq "$rollback_actor" "1" "the claim-rollback is labelled story.sh:dispatch-rollback"

# The point of the pair: same command, different actor. A verb-only record
# would have collapsed these two rows into one indistinguishable pair.
commands=$(printf '%s' "$log" | jq -r '[.log[] | select(.actor != null) | .command] | unique | join(",")')
assert_eq "$commands" "set-state" "both writes are the same command — only the actor separates them"

# Nothing storyhook wrote is allowed to claim an actor it was not given: a
# plain `story new` in this repo declared nothing.
created_actor=$(printf '%s' "$log" | jq -r '[.log[] | select(.kind == "StoryCreated") | .actor] | first')
assert_eq "$created_actor" "null" "an undeclared write records no actor rather than borrowing one"

finish
