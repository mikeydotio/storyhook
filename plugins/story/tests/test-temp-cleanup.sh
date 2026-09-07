#!/usr/bin/env bash
# A fixture helper commonly runs inside command substitution so it can return
# its path. Cleanup registration must survive that subshell and reach the EXIT
# trap in the caller that sourced lib.sh.
source "$(dirname "$0")/lib.sh"

paths=$(mktemp /tmp/story-test-cleanup-paths.XXXXXX)
_TMP_REPOS+=("$paths")

bash -c '
  source "$1"
  repo=$(mk_story_repo CLN)
  install=$(mk_versioned_claude 1.2.3)
  printf "%s\n%s\n" "$repo" "$install"
' _ "$TESTS_DIR/lib.sh" >"$paths"

while IFS= read -r path; do
  [ -n "$path" ] || fail_test "temp cleanup: helper returned an empty path"
  [ ! -e "$path" ] \
    || fail_test "temp cleanup: command-substitution fixture survived its EXIT trap at $path"
done <"$paths"

assert_eq "$(wc -l <"$paths" | tr -d " ")" "2" \
  "temp cleanup: both command-substitution helpers were exercised"

# Full Auto's real endpoint can outlive the helper invocation long enough to
# create its project-named tmux session. The shared EXIT trap must remove only
# sessions the fixture explicitly registered; a broad story-test-* sweep could
# destroy a concurrent test's live terminal.
session_state=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$session_state")
FAKE_TMUX_STATE="$session_state" \
  FAKE_TMUX_SESSIONS="story-test-owned story-test-keep" \
  PATH="$TESTS_DIR/fakes:$PATH" bash -c '
    source "$1"
    tmux has-session -t story-test-owned
    _register_tmp_tmux_session story-test-owned
  ' _ "$TESTS_DIR/lib.sh"

assert_eq "$(cat "$session_state/sessions")" "story-test-keep" \
  "temp cleanup: registered tmux session removed and unregistered session preserved"

# Teardown must retain the test body's failure when cleanup succeeds.
failure_state=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$failure_state")
FAKE_TMUX_STATE="$failure_state" FAKE_TMUX_SESSIONS="story-test-failed-body" \
  PATH="$TESTS_DIR/fakes:$PATH" bash -c '
    source "$1"
    tmux has-session -t story-test-failed-body
    _register_tmp_tmux_session story-test-failed-body
    exit 23
  ' _ "$TESTS_DIR/lib.sh"
failure_status=$?
assert_eq "$failure_status" "23" \
  "temp cleanup: preserves an existing test failure"

# Conversely, failed cleanup must fail a body that otherwise passed.
cleanup_failure_state=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$cleanup_failure_state")
FAKE_TMUX_STATE="$cleanup_failure_state" \
  FAKE_TMUX_SESSIONS="story-test-stubborn" \
  FAKE_TMUX_FAIL_KILL_SESSION=1 PATH="$TESTS_DIR/fakes:$PATH" bash -c '
    source "$1"
    tmux has-session -t story-test-stubborn
    _register_tmp_tmux_session story-test-stubborn
  ' _ "$TESTS_DIR/lib.sh" >/dev/null 2>&1
cleanup_failure_status=$?
assert_eq "$cleanup_failure_status" "1" \
  "temp cleanup: failed session cleanup fails a successful test body"

finish
