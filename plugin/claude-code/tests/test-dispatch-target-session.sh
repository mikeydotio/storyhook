#!/usr/bin/env bash
# STORY_TARGET_SESSION into an EXISTING session, from OUTSIDE tmux ($TMUX and
# $TMUX_PANE both unset). This is the non-interactive-caller seam
# STORY_TARGET_SESSION exists for (story.sh:471-473) — the seam SH-50's
# dashboard daemon relies on — but until now it had zero test coverage: every
# dispatch test faked $TMUX/$TMUX_PANE instead. STORY_CREATE_SESSION is
# deliberately NOT set here, isolating "dispatch into a session that is
# already there" from SH-50's separate creation feature (see
# test-dispatch-create-session.sh).
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"
export FAKE_TMUX_STATE
FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$FAKE_TMUX_STATE")

repo=$(mk_story_repo)
id=$(new_story "$repo" "Dispatched from outside tmux into a named session")

out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      FAKE_TMUX_SESSIONS="editor-room" \
      STORY_TARGET_SESSION="editor-room" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id" 2>&1
)

assert_eq "$(jqf "$out" .ok)" "true" "target-session: ok:true with no \$TMUX/\$TMUX_PANE at all"
assert_eq "$(jqf "$out" .claimed)" "true" "target-session: claimed:true"
assert_eq "$(jqf "$out" .session)" "editor-room" "target-session: reports the session it targeted"
assert_eq "$(jqf "$out" .session_created)" "false" "target-session: did not create a session that already existed"

# The real side effect: the window opened INTO the named session, not the
# caller's own (nonexistent) tmux context.
grep -q -- '-t editor-room:' "$FAKE_TMUX_STATE/new_window_args.log" \
  || fail_test "target-session: new-window was not targeted at editor-room:"

# tmux new-session must never have been called — the session already existed.
[ -f "$FAKE_TMUX_STATE/new_session_calls" ] \
  && fail_test "target-session: new-session ran even though the session pre-existed"

[ -d "$repo/.claude/worktrees/$id" ] || fail_test "target-session: worktree directory missing"

finish
