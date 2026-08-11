#!/usr/bin/env bash
# SH-226 regression families. The repro (test-dispatch-pane-readiness.sh) covers
# the one field scenario; these cover the DEFECT CLASS its ODC trigger names --
# startup/restart on the error path, secondary configuration.
#
# Family A  the launch never started (the field cause)
# Family B  SH-230's own claim, checked directly: a launch keystroke failure
#           knob that used to matter no longer reaches the dispatch path at
#           all, because there is no longer a launch keystroke to fail
# Family C  occupant x fixture matrix -- including the fake's own documented
#           "must NOT confirm" obligations, which nothing enforced before now
# Family D  delivery-phase matrix -- the first coverage send_prompt_confirmed's
#           receipt/submission distinction has ever had
# Family E  the configuration trigger -- the process-pattern escape hatch
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# dispatch_run <extra-env...> -- run one dispatch of a fresh story in a fresh
# repo against the fake tmux, leaving its JSON in $out. Every knob is passed
# through the documented environment seam; no production file is touched.
#
# Deliberately NOT called in a command substitution: $RUN_REPO, $RUN_ID and
# $FAKE_TMUX_STATE have to survive the call so the assertions can read the
# fake's own state and the story's real state afterwards.
dispatch_run() {
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  unset FAKE_TMUX_SESSIONS FAKE_TMUX_FAIL_NEW_SESSION FAKE_TMUX_DROP_PASTE \
        FAKE_TMUX_ENTER_ABSORB FAKE_TMUX_LAUNCH_MANGLE FAKE_TMUX_PANE_COMMAND \
        FAKE_TMUX_FAIL_SEND_KEYS FAKE_TMUX_CAPTURE
  RUN_REPO=$(mk_story_repo)
  RUN_ID=$(new_story "$RUN_REPO" "Occupant-gate case")
  out=$(
    cd "$RUN_REPO" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        STORY_READY_ATTEMPTS=4 STORY_CONFIRM_ATTEMPTS=2 \
        env "$@" bash "$SCRIPT" dispatch "$RUN_ID" --auto 2>&1
  )
}

state_of() { (cd "$RUN_REPO" && story show "$RUN_ID" --json | jq -r '.story.story.state'); }
submits() { cat "$FAKE_TMUX_STATE/prompt_submits" 2>/dev/null || echo 0; }

# ---- Family A: the launch never started (the SH-226 field cause) -------------
dispatch_run FAKE_TMUX_CAPTURE=structural FAKE_TMUX_LAUNCH_MANGLE=1
assert_eq "$(jqf "$out" .ok)" "false" "A: a pane that never became claude is refused"
assert_eq "$(jqf "$out" .reason)" "pane-not-ready" "A: refusal names the pane"
# SH-231: dispatch now gates on sentinel existence, not rendered content -- a
# launch that never became claude/node never runs a SessionStart hook, so
# "no-sentinel" (nothing ever started here) is the correct reason, not
# "wrong-process" (which now means a real sentinel exists but THIS pane's
# occupant doesn't match it -- see Family E below for that scenario).
assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" "A: and says nothing ever published a sentinel"
assert_eq "$(submits)" "0" "A: NOTHING was typed into that pane"
assert_eq "$(state_of)" "todo" "A: the claim was rolled back"
case "$(jqf "$out" .pane_tail)" in
  *"command not found"*) ;;
  *) fail_test "A: the refusal must carry the pane's own evidence (the command-not-found line)" ;;
esac

# ---- Family B: the launch keystroke knob no longer reaches dispatch ---------
# Before SH-230, story.sh typed the launch command via `paste_text` (`tmux
# send-keys -l`), so FAKE_TMUX_FAIL_SEND_KEYS=literal made that typing fail
# and the dispatch was refused -- this used to assert exactly that. SH-230
# execs the launch as new-window's own trailing argument instead: there is no
# more `send-keys -l` call on the dispatch path for it to fail. The prompt
# handoff was already delivered via `paste-buffer` (never `send-keys -l`), so
# with the launch also off that path, the knob is now inert for dispatch
# end-to-end -- only cmd_doctor's still-typed scratch-window launch can still
# trigger it. Asserting the dispatch SUCCEEDS despite the knob being armed is
# the actual proof that claim: if `send-keys -l` were reachable anywhere on
# this path, paste_text's own `|| return 1` would surface as a failure here.
dispatch_run FAKE_TMUX_CAPTURE=structural FAKE_TMUX_FAIL_SEND_KEYS=literal
assert_eq "$(jqf "$out" .ok)" "true" \
  "B: a launch-keystroke-failure knob no longer reaches dispatch -- the launch is execed, not typed"
assert_eq "$(state_of)" "in-progress" "B: the story is claimed normally"

# ---- Family C: occupant x fixture matrix ------------------------------------
# A claude occupant with a drifted footer must STILL confirm via the structural
# tier -- the fix must not have broken the tier it narrowed.
dispatch_run FAKE_TMUX_CAPTURE=structural
assert_eq "$(jqf "$out" .ok)" "true" "C: claude + drifted footer still confirms structurally"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "C: ...and says so"
assert_eq "$(submits)" "1" "C: ...and the prompt is delivered exactly once"

# The fake documents `modal` and `busy` as "must NOT confirm readiness". Nothing
# enforced that until now -- with a shell occupant they must refuse.
for fixture in modal busy churn; do
  dispatch_run FAKE_TMUX_CAPTURE=$fixture FAKE_TMUX_LAUNCH_MANGLE=1
  assert_eq "$(jqf "$out" .ok)" "false" "C: fixture '$fixture' on a shell pane must not confirm"
  assert_eq "$(submits)" "0" "C: fixture '$fixture' must not deliver the charter"
done

# ---- Family D: delivery-phase matrix ----------------------------------------
# The pane IS claude, but the paste never lands. Nothing may be submitted --
# this is the invariant SUBMIT-AFTER-RECEIPT, and before SH-226 an Enter was
# pressed here regardless.
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_DROP_PASTE=1
assert_eq "$(jqf "$out" .ok)" "false" "D: an undelivered prompt fails the dispatch"
assert_eq "$(jqf "$out" .reason)" "handoff-undelivered" "D: named as undelivered"
assert_eq "$(jqf "$out" .delivery_phase)" "undelivered" "D: phase recorded"
assert_eq "$(submits)" "0" "D: no Enter is sent when receipt was never observed"
assert_eq "$(state_of)" "todo" "D: undelivered is the one send failure safe to roll back"

# The paste LANDED but the Enter is swallowed every time: the charter may be in
# front of a live agent, so the claim must SURVIVE -- rolling back here would
# hand the same story to a second session.
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_ENTER_ABSORB=9
assert_eq "$(jqf "$out" .ok)" "false" "D: an unconfirmed submission fails the dispatch"
assert_eq "$(jqf "$out" .reason)" "handoff-unconfirmed" "D: named as unconfirmed"
assert_eq "$(jqf "$out" .delivery_phase)" "received-unsubmitted" "D: phase recorded"
assert_eq "$(state_of)" "in-progress" \
  "D: the claim is DELIBERATELY kept -- the agent may already be working"
assert_eq "$(jqf "$out" .claimed)" "true" "D: and the result says so"

# ---- Family E: the configuration trigger ------------------------------------
# SH-231 narrows what the escape hatch can rescue. It used to restore
# pre-SH-226 semantics outright: readiness was a rendered-content check, and
# `.` just turned off the process-name gate SH-226 ANDed onto it, so ANY
# stable content -- even a bare shell prompt -- could confirm. Readiness is
# now sentinel existence FIRST: a launch that never became claude/node never
# runs a SessionStart hook, so no sentinel is EVER published for it, and no
# occupant-name pattern can manufacture evidence that was never produced.
# `.` still has real power -- it is the occupant-name check specifically,
# now gated BEHIND a real sentinel rather than in front of rendered content.
dispatch_run FAKE_TMUX_CAPTURE=structural FAKE_TMUX_LAUNCH_MANGLE=1 \
        STORY_READY_PROCESS_PATTERN=.
assert_eq "$(jqf "$out" .ok)" "false" \
  "E: the escape hatch cannot rescue a launch that never published a sentinel -- there is nothing to relax"
assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" \
  "E: ...it fails for the same reason Family A does, unaffected by the pattern"

# The escape hatch DOES still rescue a real sentinel whose pane's occupant
# name the default pattern refuses -- the scenario `.` was actually meant for.
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_PANE_COMMAND=some-unlisted-wrapper \
        STORY_READY_PROCESS_PATTERN=.
assert_eq "$(jqf "$out" .ok)" "true" \
  "E: STORY_READY_PROCESS_PATTERN=. still rescues an unrecognised occupant NAME, once a sentinel is real"

# An operator whose claude reports an unexpected name can name it.
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_PANE_COMMAND=claude-wrapper \
        STORY_READY_PROCESS_PATTERN='^claude-wrapper$' 
assert_eq "$(jqf "$out" .ok)" "true" "E: a custom occupant pattern is honoured"

# ...and the default rejects it, so the knob is load-bearing rather than cosmetic.
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_PANE_COMMAND=claude-wrapper
assert_eq "$(jqf "$out" .ok)" "false" "E: the default pattern does not match it"
case "$(jqf "$out" .display)" in
  *STORY_READY_PROCESS_PATTERN*) ;;
  *) fail_test "E: a fail-closed gate must print the knob that unsticks it" ;;
esac

finish
