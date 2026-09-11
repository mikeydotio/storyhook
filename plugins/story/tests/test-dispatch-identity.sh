#!/usr/bin/env bash
# SH-677: failed registration rolls back before the charter can be delivered.
source "$(dirname "$0")/lib.sh"
repo=$(mk_story_repo)
id=$(new_story "$repo" "Registration failure")
out=$(cd "$repo" && PATH="$TESTS_DIR/fakes:$PATH" TMUX=fake TMUX_PANE=%0 \
  STORY_READY_DELAY=0 STORY_CONFIRM_DELAY=0 FAKE_TMUX_CAPTURE=marker \
  FAKE_TMUX_FAIL_IDENTITY_WRITE=1 bash "$SCRIPT" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" false "registration refuses"
assert_eq "$(jqf "$out" .reason)" pane-identity-unavailable "typed registration refusal"
assert_eq "$(jqf "$out" .claimed)" false "registration failure releases claim"
assert_contains "$(jqf "$out" .display)" "No story charter was delivered" "failure names delivery boundary"
[ ! -e "$repo/.claude/worktrees/$id" ] || fail_test "registration failure leaked worktree"
[ ! -s "$FAKE_TMUX_STATE/pastes.log" ] || fail_test "registration failure pasted the charter"
pid=$(cat "$FAKE_TMUX_STATE/stopped_pid")
[ -n "$pid" ] || fail_test "registration rollback did not stop its pane"
if kill -0 "$pid" 2>/dev/null; then fail_test "registration failure left owned agent alive"; fi
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r .story.story.state)" todo \
  "registration rollback matches store"
finish
