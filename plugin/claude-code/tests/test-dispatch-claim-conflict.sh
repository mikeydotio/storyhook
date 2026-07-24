#!/usr/bin/env bash
# A concurrent claim winning the race between story.sh's own state read and
# its claim attempt (the TOCTOU window --if-state exists to close) must
# refuse with reason:"claim-conflict" and create NO worktree/window — never
# silently retried or overwritten. Not reproducible black-box with genuine
# concurrency in a sequential test script, so this uses
# fakes/story-conflict/story: a thin wrapper that proxies every call to the
# REAL story binary except one exact `move <id> in-progress --if-state
# <state> --json`, which it answers with a canned conflict response instead
# — simulating another dispatch having just won the race a moment before
# story.sh's own claim attempt runs. Deliberately kept in its OWN
# subdirectory, not alongside fakes/tmux: prepending a directory containing
# a file literally named `story` to PATH would shadow the real binary for
# every OTHER test that also wants the fake tmux but needs a genuine
# `story` CLI underneath it.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Raced story")
# Confirm (and capture) the state story.sh will read and pass as --if-state.
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')

real_story=$(command -v story)
out=$(
  cd "$repo" \
    && PATH="$TESTS_DIR/fakes/story-conflict:$PATH" \
      STORY_REAL_BIN="$real_story" \
      STORY_CONFLICT_ID="$id" STORY_CONFLICT_EXPECTED="$state" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      bash "$SCRIPT" dispatch "$id" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "conflict: ok:false"
assert_eq "$(jqf "$out" .reason)" "claim-conflict" "conflict: reason is claim-conflict, not a generic failure"
assert_contains "$(jqf "$out" .display)" "another dispatch likely won the race" "conflict: display names the cause"

# No worktree/window ever created -- the claim step runs BEFORE any of that.
[ -d "$repo/.claude/worktrees" ] && fail_test "conflict: a worktree was created despite the lost race"

# The REAL story CLI (not the fake) still reports the original, untouched
# state -- the fake's canned response never actually wrote anything.
real_state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$real_state" "$state" "conflict: real story state is untouched by the fake's canned response"

finish
