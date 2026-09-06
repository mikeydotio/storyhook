#!/usr/bin/env bash
# SH-490: once a prompt may be in front of a live agent, an audit-write
# failure must preserve the claim and resources. Rolling them back would put
# the same story back in the ready queue while the dispatched agent may work.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"
export FAKE_TMUX_STATE
FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")

repo=$(mk_story_repo)
id=$(new_story "$repo" "Dispatch whose audit write fails")
real_story=$(command -v story)

status=0
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      STORY_BIN="$TESTS_DIR/fakes/story-comment-failure" \
      STORY_REAL_BIN="$real_story" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch --next 2>&1
) || status=$?

[ "$status" -ne 0 ] || fail_test "comment failure: helper exited successfully"
assert_eq "$(jqf "$out" .ok)" "false" "comment failure: ok:false"
assert_eq "$(jqf "$out" .reason)" "dispatch-comment-failed" \
  "comment failure: typed reason"
assert_contains "$(jqf "$out" .display)" "claim, worktree, and tmux window were left in place" \
  "comment failure: preservation is explicit"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" \
  "in-progress" "comment failure: claim remains held"
[ -d "$repo/.claude/worktrees/$id" ] \
  || fail_test "comment failure: worktree was rolled back"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$id") \
  || fail_test "comment failure: branch was rolled back"
comments=$(cd "$repo" && story show "$id" --json \
  | jq -r '[.story.story.comments[].text] | join("|")')
assert_eq "$comments" "" "comment failure: no success record was fabricated"

finish
