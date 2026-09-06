#!/usr/bin/env bash
# SH-571: autonomous Codex dispatch must prove the exact Storyhook hook package
# ran before the helper can deliver a prompt. Attended Codex remains screen-gated.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"
FAKE_BIN=$(mktemp -d /tmp/story-test-codex-binding-bin.XXXXXX)
_TMP_REPOS+=("$FAKE_BIN")
printf '#!/bin/sh\nexit 0\n' >"$FAKE_BIN/codex"
chmod +x "$FAKE_BIN/codex"

dispatch_case() {
  local mode="$1" root="$2" autonomy="$3"
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-codex-binding-tmux.XXXXXX)
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  RUN_REPO=$(mk_story_repo CBI)
  RUN_ID=$(new_story "$RUN_REPO" "Codex hook binding $mode $autonomy")
  out=$(
    cd "$RUN_REPO" &&
      PATH="$FAKE_BIN:$FAKE_TMUX_DIR:$PATH" \
      TMUX=fake TMUX_PANE=%0 STORY_AGENT=codex \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 STORY_READY_ATTEMPTS=2 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker FAKE_TMUX_CODEX_SENTINEL_MODE="$mode" \
      FAKE_TMUX_CODEX_PLUGIN_ROOT="$root" \
      bash "$SCRIPT" dispatch "$RUN_ID" $autonomy 2>&1
  )
}

state_of() {
  (cd "$RUN_REPO" && story show "$RUN_ID" --json | jq -r '.story.story.state')
}

dispatch_case missing "" --auto
assert_eq "$(jqf "$out" .ok)" "false" "missing hook: autonomous dispatch refuses"
assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" \
  "missing hook: absence has a distinct readiness reason"
assert_eq "$(state_of)" "todo" "missing hook: claim rolls back"

dispatch_case legacy "" --auto
assert_eq "$(jqf "$out" .ok)" "false" "legacy sentinel: autonomous dispatch refuses"
assert_eq "$(jqf "$out" .wait_ready_reason)" "hook-identity-missing" \
  "legacy sentinel: missing package identity is explicit"
assert_eq "$(state_of)" "todo" "legacy sentinel: claim rolls back"

dispatch_case identity /wrong/storyhook/plugin --auto
assert_eq "$(jqf "$out" .ok)" "false" "wrong hook: autonomous dispatch refuses"
assert_eq "$(jqf "$out" .wait_ready_reason)" "hook-identity-mismatch" \
  "wrong hook: package mismatch is explicit"
assert_eq "$(state_of)" "todo" "wrong hook: claim rolls back"

dispatch_case identity "$PLUGIN_ROOT" --auto
assert_eq "$(jqf "$out" .ok)" "true" "exact hook: Auto dispatch succeeds"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "exact hook: readiness confirms"

dispatch_case identity "$PLUGIN_ROOT" "--auto --full-auto"
assert_eq "$(jqf "$out" .ok)" "true" "exact hook: Full Auto dispatch succeeds"

dispatch_case missing "" ""
assert_eq "$(jqf "$out" .ok)" "true" "attended Codex remains screen-gated"
[ ! -e "$RUN_REPO/.codex/worktrees/$RUN_ID/.claude/dispatch-sentinel.json" ] \
  || fail_test "attended Codex unexpectedly published a sentinel"

finish
