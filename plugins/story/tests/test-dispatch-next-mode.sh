#!/usr/bin/env bash
# SH-344's NEXT MODE: `story.sh dispatch --next` claims whatever
# `story claim --next` picks, atomically, rather than a caller-named id.
# Same happy-path shape as test-dispatch-happy.sh, but exercised against the
# REAL story CLI's `claim --next` primitive rather than a caller-supplied id
# — a fake would risk drifting from `is_claimable`'s actual priority/number
# ordering, exactly the thing the atomic claim exists to get right.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

dispatch_next_real() {
  local dir="$1"
  (
    cd "$dir" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch --next 2>&1
  )
}

repo=$(mk_story_repo)
low_id=$(new_story "$repo" "Lower-priority story")
(cd "$repo" && story prioritize "$low_id" low >/dev/null)
high_id=$(new_story "$repo" "Higher-priority story")
(cd "$repo" && story prioritize "$high_id" high >/dev/null)

out=$(dispatch_next_real "$repo")
assert_eq "$(jqf "$out" .ok)" "true" "next-mode happy: ok:true"
assert_eq "$(jqf "$out" .claimed)" "true" "next-mode happy: claimed:true"
assert_eq "$(jqf "$out" .id)" "$high_id" "next-mode happy: the higher-priority story was picked, not the first created"
assert_eq "$(jqf "$out" .state)" "in-progress" "next-mode happy: state is in-progress"
assert_eq "$(jqf "$out" .window_name)" "$high_id" "next-mode happy: window name is the claimed story's bare id"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "next-mode happy: readiness confirmed via the fake tmux marker tier"
assert_eq "$(jqf "$out" .prompt_confirmed)" "true" "next-mode happy: prompt confirmed submitted"

# Real side effects actually happened, on the CLAIMED story, not the
# lower-priority one that was never named by the caller.
[ -d "$repo/.claude/worktrees/$high_id" ] || fail_test "next-mode happy: worktree directory missing"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$high_id") \
  || fail_test "next-mode happy: worktree branch missing"
claimed_state=$(cd "$repo" && story show "$high_id" --json | jq -r '.story.story.state')
assert_eq "$claimed_state" "in-progress" "next-mode happy: story CLI itself confirms the claim"
[ -d "$repo/.claude/worktrees/$low_id" ] && fail_test "next-mode happy: the UNCLAIMED lower-priority story got a worktree too"
low_state=$(cd "$repo" && story show "$low_id" --json | jq -r '.story.story.state')
assert_eq "$low_state" "todo" "next-mode happy: the lower-priority story was left untouched"

finish
