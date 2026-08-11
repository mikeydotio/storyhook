#!/usr/bin/env bash
# SH-230 (SH-227's R1): the launch command is EXECED as new-window's own
# trailing argument, not typed into an already-running shell afterward. These
# tests pin the mechanism itself, independent of test-dispatch-occupant-gate.sh
# (which covers the SH-226 defect-class regressions this mechanism change
# happens to also satisfy) and test-dispatch-happy.sh (the ordinary
# end-to-end path).
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

export FAKE_TMUX_STATE
FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$FAKE_TMUX_STATE")
unset FAKE_TMUX_SESSIONS FAKE_TMUX_FAIL_NEW_SESSION FAKE_TMUX_DROP_PASTE \
      FAKE_TMUX_ENTER_ABSORB FAKE_TMUX_LAUNCH_MANGLE FAKE_TMUX_PANE_COMMAND \
      FAKE_TMUX_FAIL_SEND_KEYS FAKE_TMUX_CAPTURE

repo=$(mk_story_repo)
id=$(new_story "$repo" "Exec launch mechanism")

out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id" 2>&1
)

assert_eq "$(jqf "$out" .ok)" "true" "exec-launch: an ordinary dispatch still succeeds"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "exec-launch: readiness confirms"

# --- The launch never went through send-keys/paste_text -- it is new-window's
#     OWN trailing argument, logged as part of that call and nowhere else. ---
launch_line="claude --permission-mode plan --model opusplan"
case "$(cat "$FAKE_TMUX_STATE/new_window_args.log" 2>/dev/null || true)" in
  *"$launch_line"*)
    fail_test "exec-launch: the launch command must be stripped from the logged new-window args (it is consumed as the trailing exec argument, not a flag)"
    ;;
esac

# Exactly ONE submission happened (the prompt) -- the launch was never a
# typed, submitted line, so prompt_submits counts only the real handoff.
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits" 2>/dev/null || echo 0)" "1" \
  "exec-launch: only the prompt submits -- the launch is execed, not typed and entered"

# --- The pane's occupant was derived from the exec'd launch line itself,
#     immediately -- not from a later typed-and-submitted line. ---
assert_eq "$(cat "$FAKE_TMUX_STATE/pane_command" 2>/dev/null || true)" "claude" \
  "exec-launch: the occupant is derived from the exec'd launch argument"

# --- SH-230's own design decisions actually reached tmux: remain-on-exit (the
#     diagnostic-tail preservation), and both rename pins (the title-race
#     narrowing), all targeting the exact window this dispatch opened. ---
opts="$(cat "$FAKE_TMUX_STATE/window_options.log" 2>/dev/null || true)"
assert_contains "$opts" "-t $id remain-on-exit on" \
  "exec-launch: remain-on-exit on was requested for this dispatch's window"
assert_contains "$opts" "-t $id automatic-rename off" \
  "exec-launch: automatic-rename off was requested"
assert_contains "$opts" "-t $id allow-rename off" \
  "exec-launch: allow-rename off was requested"

# --- The pane's pid is captured and exposed on the result -- SH-231 (R2) is
#     meant to consume it as an authoritative identity instead of an
#     unconfirmed hook payload; this only holds if it is really there. ---
pane_pid="$(jqf "$out" .pane_pid)"
case "$pane_pid" in
  ''|null) fail_test "exec-launch: pane_pid must be present on a successful dispatch result" ;;
  *[!0-9]*) fail_test "exec-launch: pane_pid must be numeric, got [$pane_pid]" ;;
esac

# --- A launch that never becomes a live claude/node process is still refused
#     exactly like before (FAKE_TMUX_LAUNCH_MANGLE's contract, unchanged) --
#     exec doesn't turn off the process gate SH-226 added; it only changes
#     how the launch is delivered. ---
FAKE_TMUX_STATE2="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$FAKE_TMUX_STATE2")
id2=$(new_story "$repo" "Exec launch that never becomes claude")
out2=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 STORY_READY_ATTEMPTS=4 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_STATE="$FAKE_TMUX_STATE2" \
      FAKE_TMUX_CAPTURE=structural FAKE_TMUX_LAUNCH_MANGLE=1 \
      bash "$SCRIPT" dispatch "$id2" 2>&1
)
assert_eq "$(jqf "$out2" .ok)" "false" "exec-launch: a launch that never becomes claude/node is still refused"
# SH-231: readiness is no longer a rendered-content check the process gate sits
# on top of -- it is sentinel existence itself. A launch that never became
# claude/node never runs a SessionStart hook, so no sentinel is ever
# published; the reason is "no-sentinel" (nothing ever started here), not
# "wrong-process" (a real session's sentinel exists, but this pane's occupant
# doesn't match it) -- see test-dispatch-occupant-gate.sh Family A/E for the
# fuller before/after of this reason taxonomy shift.
assert_eq "$(jqf "$out2" .wait_ready_reason)" "no-sentinel" \
  "exec-launch: a launch that never starts claude never publishes a sentinel either"

finish
