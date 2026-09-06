#!/usr/bin/env bash
# SH-564: dashboard dispatch may resolve this checkout's helper even when the
# StoryHook Claude plugin is not registered globally. A built-in Claude launch
# must therefore bind the same plugin root explicitly; otherwise its required
# SessionStart hook never publishes the readiness sentinel and the prompt is
# correctly withheld after the claim is rolled back.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes/plugin-binding"

unset STORY_LAUNCH_CMD STORY_FULL_AUTO_LAUNCH_CMD FAKE_TMUX_SUPPRESS_SENTINEL
unset FAKE_TMUX_SESSIONS FAKE_TMUX_FAIL_NEW_SESSION FAKE_TMUX_LAUNCH_MANGLE

repo=$(mk_story_repo PLG)
id=$(new_story "$repo" "Claude dispatch binds the StoryHook plugin")

out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_ATTEMPTS=3 STORY_READY_DELAY=0 \
      STORY_READY_FALLBACK_DELAY=0 STORY_CONFIRM_DELAY=0 \
      STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id" --auto 2>&1
)

ok=$(jqf "$out" .ok)
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
launch=$(cat "$FAKE_TMUX_STATE/plugin_binding_launch" 2>/dev/null || printf '')
binding=$(cat "$FAKE_TMUX_STATE/plugin_binding_status" 2>/dev/null || printf 'unobserved')

if [ "$ok" != true ]; then
  # These are fixture sanity checks, not the desired behavior. They ensure the
  # red test is SH-564's exact failure rather than an unrelated setup error.
  assert_eq "$(jqf "$out" .reason)" "pane-not-ready" \
    "missing binding: dispatch reaches the readiness refusal"
  assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" \
    "missing binding: the absent SessionStart sentinel is the failure signal"
  assert_eq "$state" "todo" \
    "missing binding: the failed dispatch rolls its claim back"
  fail_test "built-in Claude dispatch failed before prompt delivery: pane-not-ready/no-sentinel with claim rolled back"
else
  assert_eq "$(jqf "$out" .readiness_confirmed)" "true" \
    "bound launch: SessionStart readiness is confirmed"
  assert_eq "$state" "in-progress" \
    "bound launch: successful dispatch retains its claim"
fi

assert_eq "$binding" "exact" \
  "built-in Claude launch: --plugin-dir names this helper's plugin root"
assert_contains "$launch" "$PLUGIN_ROOT" \
  "built-in Claude launch: recorded argv contains the exact plugin root"

# A same-shaped but wrong plugin path must not satisfy the fixture. This also
# protects the wholesale-override contract: StoryHook never rewrites an expert
# command to rescue it from pointing at the wrong integration.
wrong_tmux_state="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$wrong_tmux_state")
wrong_repo=$(mk_story_repo WRP)
wrong_id=$(new_story "$wrong_repo" "Wrong Claude plugin binding")
wrong_out=$(
  cd "$wrong_repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      FAKE_TMUX_STATE="$wrong_tmux_state" FAKE_TMUX_CAPTURE=marker \
      STORY_READY_ATTEMPTS=3 STORY_READY_DELAY=0 \
      STORY_READY_FALLBACK_DELAY=0 STORY_CONFIRM_DELAY=0 \
      STORY_PASTE_SETTLE_DELAY=0 \
      STORY_LAUNCH_CMD="claude --plugin-dir '/tmp/not-storyhook' --permission-mode plan" \
      bash "$SCRIPT" dispatch "$wrong_id" --auto 2>&1
)
assert_eq "$(jqf "$wrong_out" .ok)" "false" \
  "wrong binding: a different plugin root cannot publish StoryHook readiness"
assert_eq "$(jqf "$wrong_out" .wait_ready_reason)" "no-sentinel" \
  "wrong binding: failure remains the missing StoryHook SessionStart witness"
assert_eq "$(cat "$wrong_tmux_state/plugin_binding_status")" "wrong-root" \
  "wrong binding: fixture distinguishes a wrong path from a missing flag"
assert_eq "$(cd "$wrong_repo" && story show "$wrong_id" --json | jq -r '.story.story.state')" "todo" \
  "wrong binding: the failed override rolls its claim back"

finish
