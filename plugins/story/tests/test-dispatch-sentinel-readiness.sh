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
        FAKE_TMUX_PANE_LIFETIME FAKE_TMUX_SENTINEL_DELAY_SECS FAKE_TMUX_SENTINEL_ON_PROBE FAKE_TMUX_EXIT_ON_REPROBE
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
assert_ok "$out" "false" \
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

# ---- pid-exited: terminate the owned process at readiness's first PID
#      reprobe, after dispatch captured its incarnation. A short wall-clock
#      lifetime can instead expire before capture under machine load. -------
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_SUPPRESS_SENTINEL=1 \
        FAKE_TMUX_EXIT_ON_REPROBE=1 \
        STORY_READY_DELAY=0.3 STORY_READY_ATTEMPTS=5
assert_ok "$out" "false" "pid-exited: a process that dies mid-poll is refused"
assert_eq "$(jqf "$out" .wait_ready_reason)" "pid-exited" \
  "pid-exited: named as the process having died, not a timeout"
assert_eq "$(state_of)" "in-progress" "pid-exited: uncertain descendant ownership preserves the claim"
# The launch-token check now refuses before an ancestry census can run. Its
# OS-specific process-read error varies, but the preserved authority must not.
assert_contains "$(jqf "$out" .display)" "startup cleanup could not be confirmed" \
  "pid-exited: unverified cleanup is reported"
assert_contains "$(jqf "$out" .display)" "claim and Git resources were preserved" \
  "pid-exited: explains why resources remain"
[ -d "$RUN_REPO/.claude/worktrees/$RUN_ID" ] \
  || fail_test "pid-exited: an unverified process tree must retain its worktree"
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits" 2>/dev/null || echo 0)" "0" \
  "pid-exited: no charter was sent after the process exited"

# ---- the happy path this whole mechanism exists to confirm, isolated from
#      Families A-E's regression framing: a real sentinel, a real live pid,
#      the default occupant pattern — ready on the very first poll. -------
dispatch_run FAKE_TMUX_CAPTURE=marker STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=3
assert_ok "$out" "true" "happy: sentinel + live pid + correct occupant confirms"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "happy: ...and says so"
assert_eq "$(state_of)" "in-progress" "happy: the story is claimed"

# A late provider event appears on the eighth readiness probe. The real
# SessionStart hook still publishes the witness. Comparing poll counts keeps
# the short/long-budget boundary independent of machine scheduling (SH-860).
readonly LATE_SENTINEL_PROBE=8
dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_SENTINEL_ON_PROBE=$LATE_SENTINEL_PROBE \
        STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=5
assert_ok "$out" "false" \
  "late sentinel: a budget shorter than the publication point refuses"
assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" \
  "late sentinel: no witness exists before the provider event"
assert_eq "$(state_of)" "todo" "late sentinel, short budget: the claim is rolled back"

dispatch_run FAKE_TMUX_CAPTURE=marker FAKE_TMUX_SENTINEL_ON_PROBE=$LATE_SENTINEL_PROBE \
        STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=12
assert_ok "$out" "true" \
  "late sentinel: a wider poll budget observes the eventual provider event"
assert_eq "$(state_of)" "in-progress" "late sentinel, wide budget: the story is claimed"
[ -f "$FAKE_TMUX_STATE/claude-hook-output.json" ] \
  || fail_test "late sentinel: the real SessionStart hook must publish the witness"

# Package identity follows the physical directory, including cache aliases.
alias_dir=$(mktemp -d /tmp/story-test-package-alias.XXXXXX)
_TMP_REPOS+=("$alias_dir")
ln -s "$PLUGIN_ROOT" "$alias_dir/cache alias"
dispatch_run FAKE_TMUX_CAPTURE=marker STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=3 \
  FAKE_TMUX_CLAUDE_SENTINEL_ROOT="$alias_dir/cache alias"
assert_ok "$out" true "Claude accepts a canonical package alias"
mkdir "$alias_dir/foreign"
dispatch_run FAKE_TMUX_CAPTURE=marker STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=3 \
  FAKE_TMUX_CLAUDE_SENTINEL_ROOT="$alias_dir/foreign"
assert_ok "$out" false "Claude refuses a different installed package"
assert_eq "$(jqf "$out" .wait_ready_reason)" hook-identity-mismatch "foreign package diagnosis"
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits" 2>/dev/null || echo 0)" 0 "foreign package receives no charter"

# The real hook's pre-RPC fallback must survive the complete Claude dispatch
# flow. Only the hook's state-home endpoint is made unavailable.
mkdir "$alias_dir/cli"
export STORY_REAL_BIN SH736_FAIL_STATE
STORY_REAL_BIN=$(command -v story)
SH736_FAIL_STATE="$alias_dir/state-is-a-file"
printf blocked > "$SH736_FAIL_STATE"
cat > "$alias_dir/cli/story" <<'WRAPPER'
#!/usr/bin/env bash
case " $* " in
  *' session-start '*) XDG_STATE_HOME="$SH736_FAIL_STATE" exec "$STORY_REAL_BIN" "$@" ;;
  *) exec "$STORY_REAL_BIN" "$@" ;;
esac
WRAPPER
chmod +x "$alias_dir/cli/story"
export PATH="$alias_dir/cli:$PATH"
dispatch_run FAKE_TMUX_CAPTURE=marker STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=3
assert_ok "$out" true "Claude dispatch accepts real degraded hook evidence"
assert_eq "$(jq -r .context_status "$RUN_REPO/.claude/worktrees/$RUN_ID/.claude/dispatch-sentinel.json")" unavailable "context failure remains explicit"
assert_contains "$(cat "$FAKE_TMUX_STATE/claude-hook-stderr")" SessionStart "cause survives the actual hook"
assert_eq "$(state_of)" in-progress "only ordinary dispatch authority claims the story"

finish
