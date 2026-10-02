#!/usr/bin/env bash
# SH-850: a resume that recreates a missing window must not be declared ready
# by the previous session's readiness witness. After a reboot the worktree
# keeps `.claude/dispatch-sentinel.json` from the lost session; the new-window
# branch used to launch over it, so `wait_ready_sentinel` passed as soon as the
# pane ran the provider, before the new session's own SessionStart hook ran,
# and continuation registration bound the old session id. The new-window launch
# now removes the stale witness first, so a replacement that never publishes
# its own is reported not ready instead of ready.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# The daemon's envelope, as test-notify-redispatch.sh documents it.
daemon_dispatch() {
  local repo="$1"
  shift
  (
    cd "$repo" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        STORY_TARGET_SESSION="$(slug_for "$repo")" STORY_CREATE_SESSION=1 \
        STORY_COUNCIL=off STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" "$@" 2>&1
  )
}

repo=$(mk_story_repo RSS)
id=$(new_story "$repo" "Resumed after its window was lost")
out=$(daemon_dispatch "$repo" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" "true" "the original dispatch succeeds"
worktree=$(jqf "$out" .worktree_path)
sentinel="$worktree/.claude/dispatch-sentinel.json"
[ -f "$sentinel" ] || fail_test "the original session published its readiness witness"

# The reboot: the window and its process are gone; the worktree and its
# witness survive.
window_id=$("$FAKE_TMUX_DIR/tmux" display-message -p -t "$(jqf "$out" .pane)" '#{window_id}')
"$FAKE_TMUX_DIR/tmux" kill-window -t "$window_id"
unset FAKE_TMUX_PANES
[ -f "$sentinel" ] || fail_test "the stale witness survives the lost window"

# The replacement session never publishes a witness of its own.
out=$(FAKE_TMUX_SUPPRESS_SENTINEL=1 STORY_READY_ATTEMPTS=3 daemon_dispatch "$repo" dispatch "$id" --resume)
assert_eq "$(jqf "$out" .ok)" "false" "a replacement without its own witness is not ready"
assert_eq "$(jqf "$out" .reason)" "pane-not-ready" "the refusal names readiness"
assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" \
  "the old session's witness no longer stands in for the new one"
[ ! -f "$sentinel" ] || fail_test "the stale witness was removed before the new window launched"

# Control: a replacement that publishes its own witness is ready.
out=$(daemon_dispatch "$repo" dispatch "$id" --resume)
assert_eq "$(jqf "$out" .ok)" "true" "a replacement that publishes its own witness is ready"
assert_eq "$(jqf "$out" .worktree_reused)" "true" "the resume reuses the surviving worktree"
[ -f "$sentinel" ] || fail_test "the replacement published a fresh witness"

finish
