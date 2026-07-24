#!/usr/bin/env bash
# Happy path: a ready story dispatches for real (non-dry-run) -- claims via
# the storyhook CAS primitive, creates the expected worktree/branch, opens
# a tmux window (fake), and hands off the prompt. Same fake-tmux harness
# style as agentics' plugins/issue/tests/test-dispatch-base.sh.
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
id=$(new_story "$repo" "Ready story for real dispatch")

out=$(dispatch_real "$repo" "$id")
assert_eq "$(jqf "$out" .ok)" "true" "happy: ok:true"
assert_eq "$(jqf "$out" .claimed)" "true" "happy: claimed:true"
assert_eq "$(jqf "$out" .state)" "in-progress" "happy: state is in-progress"
assert_eq "$(jqf "$out" .window_name)" "sto-$id" "happy: window name uses the repo-prefix"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "happy: readiness confirmed via the fake tmux marker tier"
assert_eq "$(jqf "$out" .prompt_confirmed)" "true" "happy: prompt confirmed submitted"
assert_eq "$(jqf "$out" 'has("warning")')" "false" "happy: no warning on the clean path"

# Real side effects actually happened -- not just a correct-looking JSON.
[ -d "$repo/.claude/worktrees/sto-$id" ] || fail_test "happy: worktree directory missing"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-sto-$id") \
  || fail_test "happy: worktree branch missing"
claimed_state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$claimed_state" "in-progress" "happy: story CLI itself confirms the claim"
grep -q "^\.claude/worktrees/\$" "$repo/.gitignore" 2>/dev/null \
  || fail_test "happy: .gitignore was not updated with the worktree-ignore rule"

finish
