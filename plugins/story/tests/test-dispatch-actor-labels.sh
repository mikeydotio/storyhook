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

# SH-264: this used to prepend the fake's FILE ($TESTS_DIR/fakes/tmux) rather
# than its directory. A PATH entry that is not a directory is silently
# skipped, so `tmux` resolved to the REAL binary, which failed to reach a
# socket literally named `fake` — an accident that happened to roll the claim
# back for a reason unrelated to the one this test names. Use the suite's
# normal spelling (the directory), and prove the fake actually ran.
FAKE_TMUX_DIR="$TESTS_DIR/fakes"

repo=$(mk_story_repo)
id=$(new_story "$repo" "A story to claim")

# A dispatch that claims and then fails after the claim, so both halves run in
# one go. FAKE_TMUX_LAUNCH_MANGLE=1 models the launch never becoming claude
# (SH-226's field failure): the pane's occupant stays a shell and no dispatch
# sentinel is ever published, so wait_ready_sentinel refuses and dispatch
# rolls its own claim back — the claim/rollback pair this test is about.
# STORY_READY_* bounds the poll so the refusal is immediate rather than the
# default ~15s (60 attempts x 0.25s).
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      FAKE_TMUX_LAUNCH_MANGLE=1 \
      STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=3 \
      bash "$SCRIPT" dispatch "$id" 2>&1
) || true

# Prove the ROUTE, not just the actor labels: a future accident that puts the
# real tmux back on PATH must fail here, loudly, rather than pass by
# coincidence the way this test used to.
assert_eq "$(jqf "$out" .ok)" "false" "the mangled launch is refused"
assert_eq "$(jqf "$out" .readiness_confirmed)" "false" "readiness never confirms"
assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" \
  "refused for the stated reason -- no sentinel ever published, not a window-open failure"
assert_eq "$(jqf "$out" .pane_command)" "zsh" \
  "the occupant is the mangled fallback shell, not claude"

# The assertion whose absence is what let the old PATH bug go undetected: the
# fake was actually invoked for this dispatch's new-window call.
if [ ! -s "$FAKE_TMUX_STATE/new_window_args.log" ]; then
  fail_test "the fake tmux never recorded a new-window call -- PATH did not reach it"
fi
if ! grep -q -- "-n $id" "$FAKE_TMUX_STATE/new_window_args.log"; then
  fail_test "the fake's new-window call does not name this dispatch's window"
fi

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
