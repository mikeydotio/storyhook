#!/usr/bin/env bash
# SH-231 (SH-227 R2): wait_ready_sentinel itself, the mechanics that don't fit
# test-dispatch-occupant-gate.sh's before/after framing (that file's Family A
# and E cover the reason-taxonomy shift; this file covers the two failure
# modes only the new mechanism can produce at all — no-sentinel with a
# genuinely live, correctly-named process, and pid-exited).
#
# Design of record: the council verdict on SH-231 (`story show SH-231`). Every
# success requires all three: the pane still shows the pid story.sh captured
# at window-open time, that pid is still alive (`kill -0`), AND a sentinel
# exists there with the right occupant. These tests isolate each of the first
# two from the third.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# dispatch_run <extra-env...> — same shape as test-dispatch-occupant-gate.sh's
# own helper: a fresh repo, a fresh story, a fresh $FAKE_TMUX_STATE per call.
dispatch_run() {
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  unset FAKE_TMUX_SESSIONS FAKE_TMUX_FAIL_NEW_SESSION FAKE_TMUX_DROP_PASTE \
        FAKE_TMUX_ENTER_ABSORB FAKE_TMUX_LAUNCH_MANGLE FAKE_TMUX_PANE_COMMAND \
        FAKE_TMUX_FAIL_SEND_KEYS FAKE_TMUX_CAPTURE FAKE_TMUX_SUPPRESS_SENTINEL \
        FAKE_TMUX_PANE_LIFETIME
  RUN_REPO=$(mk_story_repo)
  RUN_ID=$(new_story "$RUN_REPO" "Sentinel readiness case")
  out=$(
    cd "$RUN_REPO" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 STORY_CONFIRM_ATTEMPTS=2 \
        env "$@" bash "$SCRIPT" dispatch "$RUN_ID" --auto 2>&1
  )
}

state_of() { (cd "$RUN_REPO" && story show "$RUN_ID" --json | jq -r '.story.story.state'); }

# ---- no-sentinel: a genuinely live, correctly-named process, but its
#      SessionStart hook never publishes anything (hooks.json missing or
#      disabled) — distinct from a launch that never became claude at all
#      (test-dispatch-occupant-gate.sh's Family A). ---------------------------
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_SUPPRESS_SENTINEL=1 \
        STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=3
assert_eq "$(jqf "$out" .ok)" "false" \
  "no-sentinel: a live, correctly-named process with no published sentinel is still refused"
assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" \
  "no-sentinel: named as exactly that, not a process mismatch"
assert_eq "$(jqf "$out" .pane_command)" "claude" \
  "no-sentinel: the occupant really is claude — this is not the wrong-process case"
assert_eq "$(state_of)" "todo" "no-sentinel: the claim is rolled back"
case "$(cat "$FAKE_TMUX_STATE/prompt_submits" 2>/dev/null || echo 0)" in
  0) : ;;
  *) fail_test "no-sentinel: nothing may be typed into an unconfirmed pane" ;;
esac

# ---- pid-exited: the launched process dies mid-poll. A near-instant
#      placeholder lifetime (FAKE_TMUX_PANE_LIFETIME) plus a suppressed
#      sentinel and a real STORY_READY_DELAY between polls is what makes this
#      deterministic rather than a race against an external kill: by the
#      second poll the placeholder has already exited on its own. --------
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_SUPPRESS_SENTINEL=1 \
        FAKE_TMUX_PANE_LIFETIME=0.1 \
        STORY_READY_DELAY=0.3 STORY_READY_ATTEMPTS=5
assert_eq "$(jqf "$out" .ok)" "false" "pid-exited: a process that dies mid-poll is refused"
assert_eq "$(jqf "$out" .wait_ready_reason)" "pid-exited" \
  "pid-exited: named as the process having died, not a timeout"
assert_eq "$(state_of)" "todo" "pid-exited: the claim is rolled back"

# ---- the happy path this whole mechanism exists to confirm, isolated from
#      Families A-E's regression framing: a real sentinel, a real live pid,
#      the default occupant pattern — ready on the very first poll. -------
dispatch_run FAKE_TMUX_CAPTURE=marker STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=3
assert_eq "$(jqf "$out" .ok)" "true" "happy: sentinel + live pid + correct occupant confirms"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "happy: ...and says so"
assert_eq "$(state_of)" "in-progress" "happy: the story is claimed"

finish
