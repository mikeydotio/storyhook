#!/usr/bin/env bash
# STORY_CREATE_SESSION (SH-50) — the dashboard daemon's seam for dispatching
# into a tmux session nothing has created yet. Three cases:
#   1. session absent  -> story.sh creates it, then dispatches into it.
#   2. session present -> story.sh does NOT recreate it (no-op has-session hit).
#   3. creation fails   -> the worktree/branch/claim are rolled back exactly
#                          like a failed `new-window` already rolls them back
#                          (story.sh's existing idiom, mirrored for this new
#                          failure point).
# Every case runs from outside tmux ($TMUX/$TMUX_PANE unset) — the whole
# point of STORY_CREATE_SESSION is a caller with no tmux context of its own.
# FAKE_TMUX_STATE, FAKE_TMUX_SESSIONS and FAKE_TMUX_FAIL_NEW_SESSION are
# exported per case (matching test-doctor-capture.sh's idiom) rather than
# passed as a command prefix, so there is no ambiguity about whether the fake
# tmux — run from a subshell inside dispatch_into — actually sees them. Each
# is `unset` (never just reassigned) between cases that don't want it, since
# `unset` is what actually clears a case's env for the next one.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

dispatch_into() {
  local dir="$1" id="$2" session="$3"
  (
    cd "$dir" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        STORY_TARGET_SESSION="$session" STORY_CREATE_SESSION=1 \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch "$id" 2>&1
  )
}

# --- Case 1: session absent -> created -------------------------------------
# `unset` (not a later plain assignment) is what clears a var between cases;
# note that `unset` also drops the export attribute the top-of-file `export`
# gave it, so every later case re-exports explicitly rather than relying on
# a bare assignment to an already-unset name being seen by the fake tmux
# subprocess.
export FAKE_TMUX_STATE
FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$FAKE_TMUX_STATE")
unset FAKE_TMUX_SESSIONS FAKE_TMUX_FAIL_NEW_SESSION
repo1=$(mk_story_repo)
id1=$(new_story "$repo1" "Dispatched into a freshly created session")

out1=$(dispatch_into "$repo1" "$id1" "dash-alpha")
assert_eq "$(jqf "$out1" .ok)" "true" "create-session/absent: ok:true"
assert_eq "$(jqf "$out1" .session)" "dash-alpha" "create-session/absent: reports the target session"
assert_eq "$(jqf "$out1" .session_created)" "true" "create-session/absent: session_created:true"
assert_eq "$(cat "$FAKE_TMUX_STATE/new_session_calls" 2>/dev/null || echo 0)" "1" \
  "create-session/absent: tmux new-session ran exactly once"
grep -q -- '-t dash-alpha:' "$FAKE_TMUX_STATE/new_window_args.log" \
  || fail_test "create-session/absent: the window opened into the new session"
[ -d "$repo1/.claude/worktrees/$id1" ] || fail_test "create-session/absent: worktree directory missing"

# --- Case 2: session present -> not recreated -------------------------------
export FAKE_TMUX_STATE
FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$FAKE_TMUX_STATE")
export FAKE_TMUX_SESSIONS="dash-beta"
unset FAKE_TMUX_FAIL_NEW_SESSION
repo2=$(mk_story_repo)
id2=$(new_story "$repo2" "Dispatched into a pre-existing session")

out2=$(dispatch_into "$repo2" "$id2" "dash-beta")
assert_eq "$(jqf "$out2" .ok)" "true" "create-session/present: ok:true"
assert_eq "$(jqf "$out2" .session_created)" "false" "create-session/present: session_created:false"
[ -f "$FAKE_TMUX_STATE/new_session_calls" ] \
  && fail_test "create-session/present: tmux new-session ran even though the session already existed"

# --- Case 3: creation fails -> rollback -------------------------------------
export FAKE_TMUX_STATE
FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$FAKE_TMUX_STATE")
unset FAKE_TMUX_SESSIONS
export FAKE_TMUX_FAIL_NEW_SESSION=1
repo3=$(mk_story_repo)
id3=$(new_story "$repo3" "Dispatch whose session creation fails")

out3=$(dispatch_into "$repo3" "$id3" "dash-gamma")
assert_eq "$(jqf "$out3" .ok)" "false" "create-session/fails: ok:false"
assert_contains "$(jqf "$out3" .display)" "failed to create tmux session" \
  "create-session/fails: names the session-creation failure"
assert_contains "$(jqf "$out3" .display)" "Rolled the claim back" \
  "create-session/fails: claim was rolled back"

rolled_state=$(cd "$repo3" && story show "$id3" --json | jq -r '.story.story.state')
assert_eq "$rolled_state" "todo" "create-session/fails: story state rolled back, not stranded at in-progress"
[ -d "$repo3/.claude/worktrees/$id3" ] \
  && fail_test "create-session/fails: worktree directory was left behind"
(cd "$repo3" && git show-ref --verify --quiet "refs/heads/worktree-$id3") \
  && fail_test "create-session/fails: worktree branch was left behind"

finish
