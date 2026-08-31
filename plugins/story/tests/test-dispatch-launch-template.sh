#!/usr/bin/env bash
# The launch-command ASSEMBLY matrix for --model/--effort/--speed (SH-517):
# {claude,codex} x {attended,auto,full-auto} x {default,model,effort,fast,all}.
# Reads the composed launch command back out of the SUCCESS JSON's own
# `display` field, which already embeds it verbatim in backticks
# ("... launched $AGENT_LABEL with `$launch_cmd` (plan mode), ...",
# story.sh:1936) — the fake tmux itself discards the full launch line after
# deriving only its basename (SH-230's EXEC form), so `display` is the one
# place a test can see the exact string a real pane would exec.
#
# The one case load-bearing for the whole feature: Claude's `--settings` flag
# is not repeatable, so a fast selection on an --auto dispatch must MERGE
# `fastMode:true` into the SAME JSON object the auto template already carries
# (`{"permissions":{"defaultMode":"acceptEdits"}}`), not append a second
# --settings flag.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# dispatch_and_capture <prefix> <extra-args...> -- fresh repo, fresh story,
# fresh fake-tmux state per call (SH-263: sharing FAKE_TMUX_STATE across
# dispatches in one test corrupts the fake's model). Echoes the success
# display, which contains the composed launch command in backticks.
dispatch_and_capture() {
  local prefix="$1"; shift
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-launch-tpl-tmux.XXXXXX)
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  local repo id out
  repo=$(mk_story_repo "$prefix")
  id=$(new_story "$repo" "Launch template case $prefix")
  out=$(
    cd "$repo" \
      && PATH="$FAKE_TMUX_DIR:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch "$id" "$@" 2>&1
  )
  [ "$(jqf "$out" .ok)" = "true" ] || fail_test "$prefix: dispatch itself failed: $out"
  jqf "$out" .display
}

# --- Claude, attended -------------------------------------------------------

disp=$(dispatch_and_capture CLA)
assert_contains "$disp" 'with `claude --permission-mode plan --model opusplan`' \
  "claude attended default: unchanged from today"
case "$disp" in *--settings*) fail_test "claude attended default: unexpected --settings" ;; esac

disp=$(dispatch_and_capture CLB --model=haiku --effort=max)
assert_contains "$disp" 'with `claude --permission-mode plan --model haiku --effort max`' \
  "claude attended model+effort: composed in order"
case "$disp" in *--settings*) fail_test "claude attended model+effort: unexpected --settings" ;; esac

disp=$(dispatch_and_capture CLC --speed=fast)
assert_contains "$disp" \
  'with `claude --permission-mode plan --model opusplan --settings '"'"'{"fastMode":true}'"'"'`' \
  "claude attended fast-only: settings holds only fastMode"

disp=$(dispatch_and_capture CLD --model=sonnet --effort=low --speed=fast)
assert_contains "$disp" \
  'with `claude --permission-mode plan --model sonnet --effort low --settings '"'"'{"fastMode":true}'"'"'`' \
  "claude attended all-selectors: full composition"

# --- Claude, --auto: the --settings MERGE case ------------------------------

disp=$(dispatch_and_capture CLE --auto)
assert_contains "$disp" \
  'claude --permission-mode plan --model opusplan --settings '"'"'{"permissions":{"defaultMode":"acceptEdits"}}'"'" \
  "claude auto default: unchanged from today"

disp=$(dispatch_and_capture CLF --auto --speed=fast)
assert_contains "$disp" \
  '--settings '"'"'{"permissions":{"defaultMode":"acceptEdits"},"fastMode":true}'"'" \
  "claude auto + fast: fastMode MERGED into the same settings object, not a second --settings flag"
fast_flag_count=$(printf '%s' "$disp" | grep -o -- '--settings' | wc -l | tr -d ' ')
assert_eq "$fast_flag_count" "1" "claude auto + fast: exactly one --settings flag"

disp=$(dispatch_and_capture CLG --auto --model=opus --effort=high --speed=fast)
assert_contains "$disp" "--model opus --effort high --settings" \
  "claude auto + all selectors: model/effort still positional, settings still merged"
assert_contains "$disp" '"permissions":{"defaultMode":"acceptEdits"}' \
  "claude auto + all selectors: acceptEdits survives the merge"
assert_contains "$disp" '"fastMode":true' \
  "claude auto + all selectors: fastMode present in the same object"

# --- Claude, --full-auto: reuses the --auto template, per today's behavior --

disp=$(dispatch_and_capture CLH --auto --full-auto --speed=fast)
assert_contains "$disp" \
  '{"permissions":{"defaultMode":"acceptEdits"},"fastMode":true}' \
  "claude full-auto + fast: same merge as --auto (full-auto shares the auto template)"

# --- Codex, attended ---------------------------------------------------------

disp=$(dispatch_and_capture CDA --agent=codex)
assert_contains "$disp" 'with `codex --no-alt-screen -c check_for_update_on_startup=false`' \
  "codex attended default: unchanged from today"
case "$disp" in *"-m "*) fail_test "codex attended default: unexpected -m flag" ;; esac

disp=$(dispatch_and_capture CDB --agent=codex --model=gpt-5.6-terra --effort=low)
assert_contains "$disp" \
  'codex --no-alt-screen -c check_for_update_on_startup=false -m gpt-5.6-terra -c model_reasoning_effort="low"' \
  "codex attended model+effort: composed flatly"

disp=$(dispatch_and_capture CDC --agent=codex --speed=fast)
assert_contains "$disp" 'codex --no-alt-screen -c check_for_update_on_startup=false -c service_tier="priority"' \
  "codex attended fast: service_tier flag appended"

# --- Codex, --auto -----------------------------------------------------------

disp=$(dispatch_and_capture CDD --agent=codex --auto)
assert_contains "$disp" \
  "codex --no-alt-screen -c check_for_update_on_startup=false --approve-for-me --dangerously-bypass-hook-trust" \
  "codex auto default: unchanged from today"

disp=$(dispatch_and_capture CDE --agent=codex --auto --model=gpt-5.6-luna --speed=fast)
assert_contains "$disp" \
  'codex --no-alt-screen -c check_for_update_on_startup=false --approve-for-me --dangerously-bypass-hook-trust -m gpt-5.6-luna -c service_tier="priority"' \
  "codex auto + model + fast: fixed auto flags precede the selectors"

finish
