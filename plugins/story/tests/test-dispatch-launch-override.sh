#!/usr/bin/env bash
# $STORY_LAUNCH_CMD / $STORY_FULL_AUTO_LAUNCH_CMD are wholesale expert
# overrides (story.sh:210-243) -- an operator's own exact command line, with
# nowhere for a --model/--effort/--speed selector to be spliced in without
# guessing at its shape. SH-517 refuses the combination by name rather than
# silently ignoring the selector or corrupting the operator's command, the
# same posture the SH-511 header comment already commits to.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

dispatch_with_launch_cmd() {
  local repo="$1" id="$2" launch_cmd="$3"; shift 3
  (
    cd "$repo" \
      && PATH="$FAKE_TMUX_DIR:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
        STORY_LAUNCH_CMD="$launch_cmd" \
        bash "$SCRIPT" dispatch "$id" "$@" 2>&1
  )
}

# A selector combined with STORY_LAUNCH_CMD refuses before any side effect,
# naming the environment variable responsible.
repo=$(mk_story_repo LOA)
id=$(new_story "$repo" "STORY_LAUNCH_CMD plus a selector refuses")
out=$(dispatch_with_launch_cmd "$repo" "$id" "claude --permission-mode plan" --speed=fast)
assert_eq "$(jqf "$out" .ok)" "false" "launch override + selector: ok:false"
assert_contains "$out" "STORY_LAUNCH_CMD" "launch override + selector: names the variable"
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "todo" "launch override + selector: story never claimed"
[ ! -d "$repo/.claude/worktrees/$id" ] || fail_test "launch override + selector: worktree created anyway"

# STORY_LAUNCH_CMD alone (no selector) is untouched -- SH-517 must not
# regress the existing override contract.
repo2=$(mk_story_repo LOB)
id2=$(new_story "$repo2" "STORY_LAUNCH_CMD alone still works")
out=$(dispatch_with_launch_cmd "$repo2" "$id2" "claude --permission-mode plan --model sonnet")
assert_eq "$(jqf "$out" .ok)" "true" "launch override alone: still dispatches"

# STORY_FULL_AUTO_LAUNCH_CMD combined with a selector under --full-auto
# refuses the same way, naming ITS OWN variable (not the general one) --
# mirroring FULL_AUTO_IGNORED_GENERAL_OVERRIDE's existing separation between
# the two override seams.
repo3=$(mk_story_repo LOC)
id3=$(new_story "$repo3" "STORY_FULL_AUTO_LAUNCH_CMD plus a selector refuses")
out=$(
  cd "$repo3" \
    && PATH="$FAKE_TMUX_DIR:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
      STORY_FULL_AUTO_LAUNCH_CMD="claude --permission-mode plan" \
      bash "$SCRIPT" dispatch "$id3" --auto --full-auto --model=haiku 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "full-auto override + selector: ok:false"
assert_contains "$out" "STORY_FULL_AUTO_LAUNCH_CMD" "full-auto override + selector: names the variable"

finish
