#!/usr/bin/env bash
# SH-840: an ok assertion that fails reports the WHOLE answer.
#
# `.ok` says only THAT a verb refused. Its `reason` and `display` say WHY, and a
# gate log is the only evidence a load-dependent failure leaves behind:
# test-unclaim.sh once answered ok:false under gate load, printed nothing else,
# and the cause could not be read back from the log. `assert_ok` is the
# assertion that keeps that evidence; this file holds it to that.
source "$(dirname "$0")/lib.sh"

declare -F assert_ok >/dev/null || {
  fail_test "lib.sh defines no assert_ok"
  finish
}

# probe <answer> <expected> — run assert_ok in a subshell, so its verdict cannot
# fail THIS test, and print what it wrote followed by the verdict it recorded.
probe() {
  (
    assert_ok "$1" "$2" "probe" 2>&1
    printf 'failed=%s\n' "$_FAILED"
  )
}

refusal='{"ok":false,"reason":"resource-query-failed","display":"[story] cannot inspect TST-2: tmux timed out"}'
released='{"ok":true,"display":"[story] unclaim TST-2: released"}'

# --- a match is silent and records nothing ----------------------------------
assert_eq "$(probe "$refusal" false)" "failed=0" "match: an expected refusal passes silently"
assert_eq "$(probe "$released" true)" "failed=0" "match: an expected success passes silently"

# --- a mismatch names the answer's own reason and display --------------------
out=$(probe "$refusal" true)
assert_contains "$out" "FAIL: probe — expected [true], got [false]" \
  "refusal: the field comparison is reported as assert_eq reports it"
assert_contains "$out" "resource-query-failed" "refusal: the reason is printed"
assert_contains "$out" "[story] cannot inspect TST-2: tmux timed out" "refusal: the display is printed"
assert_contains "$out" "$refusal" "refusal: the answer is printed whole"
assert_eq "$(printf '%s\n' "$out" | grep -c '^FAIL:')" "1" "refusal: one assertion is one failure"
assert_eq "$(printf '%s\n' "$out" | tail -1)" "failed=1" "refusal: the test is marked failed"

out=$(probe "$released" false)
assert_contains "$out" "expected [false], got [true]" "success: the other direction is reported too"
assert_contains "$out" "$released" "success: and carries its answer"
assert_eq "$(printf '%s\n' "$out" | tail -1)" "failed=1" "success: the test is marked failed"

# --- an answer that is not JSON is the most important one to see -------------
# A crash, a usage line, a shell error: jq cannot read it, so `.ok` is empty and
# only the raw text says what happened.
crash='story.sh: line 4418: resource_window_snapshot: command not found'
out=$(probe "$crash" true)
assert_contains "$out" "expected [true], got []" "non-JSON: the empty field is reported"
assert_contains "$out" "answer: [$crash]" "non-JSON: the raw text is printed"
assert_eq "$(printf '%s\n' "$out" | tail -1)" "failed=1" "non-JSON: the test is marked failed"

out=$(probe "" true)
assert_contains "$out" "answer: []" "empty: an empty answer is shown as empty"
assert_eq "$(printf '%s\n' "$out" | tail -1)" "failed=1" "empty: the test is marked failed"

# --- a multi-line answer is kept whole, not cut at its first line ------------
pretty=$(printf '%s' "$refusal" | jq .)
out=$(probe "$pretty" true)
assert_contains "$out" "$pretty" "multi-line: every line of the answer is printed"

# --- a caller under `set -e` keeps running, as with assert_eq ----------------
# Several suites run `set -euo pipefail`. A failed assertion records the
# failure; it never ends the script early and hides the assertions after it.
out=$(
  set -euo pipefail
  assert_ok "$crash" true "errexit" 2>/dev/null
  assert_ok "$refusal" true "errexit" 2>/dev/null
  printf 'survived'
)
assert_eq "$out" "survived" "errexit: neither a non-JSON nor a failing answer exits the caller"

finish
