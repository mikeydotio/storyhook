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

# --- nothing asserts `.ok` through the field alone -------------------------
# SH-840 moved every ok assertion onto assert_ok (council c860955c, D1). This
# keeps the lossy shape -- assert_eq over the jq-read `.ok` field, which prints
# only that field -- from being copied back in from an older branch. Derived
# over every tracked source that calls lib.sh's assertions, never a list: this
# suite and the Rust harness's shell fixtures (tests/support/protect_*.rs).
# The shape is spelled with escapes, so neither this file nor lib.sh can match.
repo_root=$(cd "$TESTS_DIR/../../.." && pwd -P)
quote="['\"]?"
old_shape='assert_eq[[:space:]]+"\$\(jqf[[:space:]]+"[^"]*"[[:space:]]+'"$quote"'\.ok'"$quote"'\)"'

# The pattern must recognise the shape it forbids, in each spelling, and must
# not reach a nested field such as `.cleanup.ok`, which is not the verdict.
for spelling in \
  "assert_eq \"\$(jqf \"\$out\" .ok)\" true label" \
  "  assert_eq \"\$(jqf \"\${out2}\" '.ok')\" \"false\" label"; do
  printf '%s\n' "$spelling" | grep -Eq "$old_shape" \
    || fail_test "guard: the pattern no longer matches [$spelling]"
done
for spelling in \
  "assert_eq \"\$(jqf \"\$out\" .cleanup.ok)\" false label" \
  "assert_eq \"\$(jqf \"\$out\" .ok_count)\" 1 label" \
  "assert_ok \"\$out\" true label"; do
  if printf '%s\n' "$spelling" | grep -Eq "$old_shape"; then
    fail_test "guard: the pattern also matches [$spelling]"
  fi
done

status=0
offenders=$(git -C "$repo_root" grep -nE "$old_shape" -- 'plugins/story/tests/*.sh' 'tests/*.rs') || status=$?
case "$status" in
  0)
    fail_test "these ok assertions print only the field, so a failure loses the answer's reason and display (SH-840). Use assert_ok \"\$answer\" <expected> <label> from plugins/story/tests/lib.sh; to convert, run: sed -E -i '' 's/assert_eq \"\\\$\\(jqf \"(\\\$[A-Za-z_0-9]+)\" \\.ok\\)\"/assert_ok \"\\1\"/g' <file>
$offenders"
    ;;
  1) : ;;
  *) fail_test "guard: git grep could not scan $repo_root (exit $status): $offenders" ;;
esac

# Never vacuous: the scan must reach the suite it guards and the Rust fixtures.
converted=$(git -C "$repo_root" grep -l 'assert_ok "' -- 'plugins/story/tests/test-*.sh' | wc -l | tr -d ' ')
[ "$converted" -ge 80 ] \
  || fail_test "guard: only $converted plugin tests call assert_ok; the scan is not reaching the suite"
git -C "$repo_root" grep -q 'assert_ok "' -- 'tests/support/*.rs' \
  || fail_test "guard: no Rust fixture calls assert_ok; the scan is not reaching tests/support"

finish
