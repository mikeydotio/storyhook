#!/usr/bin/env bash
# Basic argument/usage validation — no repo or story state needed.
source "$(dirname "$0")/lib.sh"

out=$(bash "$SCRIPT" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "no subcommand: ok:false"
assert_contains "$(jqf "$out" .display)" "usage" "no subcommand: usage message"

out=$(bash "$SCRIPT" dispatch 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dispatch with no id: ok:false"
assert_contains "$(jqf "$out" .display)" "usage" "dispatch with no id: usage message"

out=$(bash "$SCRIPT" dispatch "bad id!" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dispatch with invalid id: ok:false"
assert_contains "$(jqf "$out" .display)" "alphanumeric" "dispatch with invalid id: names the constraint"

out=$(bash "$SCRIPT" bogus-subcommand 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unknown subcommand: ok:false"

# The usage string is the only discovery surface a caller has when it guesses
# wrong, so it must name every verb the router actually accepts. Pins the two
# from drifting apart as verbs are added.
usage=$(jqf "$(bash "$SCRIPT" bogus-subcommand 2>&1)" .display)
for verb in list view dispatch create complete doctor capture; do
  assert_contains "$usage" "$verb" "usage: names the \`$verb\` verb"
done

finish
