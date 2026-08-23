#!/usr/bin/env bash
# A story dispatched twice in a row (its worktree/branch already exists from
# the first dispatch) must refuse the second attempt with no further side
# effects, and roll the claim back so the story isn't left stranded at
# in-progress with nothing to show for it (claim_rollback_note).
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

dispatch_real() {
  local dir="$1" id="$2"
  (
    cd "$dir" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch "$id" 2>&1
  )
}

repo=$(mk_story_repo)
id=$(new_story "$repo" "Dispatched twice")

first=$(dispatch_real "$repo" "$id")
assert_eq "$(jqf "$first" .ok)" "true" "first dispatch: ok:true"

# Move it back to a ready state so the SECOND attempt reaches the
# worktree/branch collision check rather than being turned away earlier by
# the already-in-progress guard — isolating the specific path under test.
(cd "$repo" && story move "$id" todo --if-state in-progress >/dev/null)

second=$(dispatch_real "$repo" "$id")
assert_eq "$(jqf "$second" .ok)" "false" "second dispatch: ok:false (worktree/branch collision)"
assert_contains "$(jqf "$second" .display)" "already exists" "second dispatch: reason names the collision"
assert_contains "$(jqf "$second" .display)" "Rolled the claim back" "second dispatch: claim was rolled back"

# Confirm the rollback actually happened: state is really back to "todo",
# not silently stranded at in-progress with no worktree to show for it.
rolled_state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$rolled_state" "todo" "second dispatch: story state rolled back to todo, not stranded"

finish
