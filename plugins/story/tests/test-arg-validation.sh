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

# SH-62: dispatch used to silently ignore a stray trailing token; it is now a
# hard fail, and the usage string names both supported dispatch flags.
out=$(bash "$SCRIPT" dispatch SH-1 junk 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dispatch with a trailing token: ok:false"
assert_contains "$(jqf "$out" .display)" "usage" "dispatch with a trailing token: usage message"
assert_contains "$(jqf "$out" .display)" "--auto" "dispatch with a trailing token: usage names --auto"
assert_contains "$(jqf "$out" .display)" "--full-auto" "dispatch with a trailing token: usage names --full-auto"
assert_contains "$(jqf "$out" .display)" "--agent=claude|codex" "dispatch with a trailing token: usage names --agent"
assert_contains "$(jqf "$out" .display)" "--force" "dispatch with a trailing token: usage names --force"

out=$(bash "$SCRIPT" dispatch SH-1 --agent=claude-code 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dispatch with legacy agent token: ok:false"
assert_contains "$(jqf "$out" .display)" "--agent=claude" "dispatch with legacy agent token: canonical remedy"

out=$(bash "$SCRIPT" dispatch SH-1 --agent=codex --agent=claude 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dispatch with duplicate agent: ok:false"
assert_contains "$(jqf "$out" .display)" "only once" "dispatch with duplicate agent: names duplication"

out=$(bash "$SCRIPT" dispatch SH-1 --auto --auto 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dispatch with duplicate auto: ok:false"
assert_contains "$(jqf "$out" .display)" "only once" "dispatch with duplicate auto: names duplication"

out=$(bash "$SCRIPT" dispatch --next --force 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dispatch --next --force: ok:false"
assert_contains "$(jqf "$out" .display)" "requires a named story id" \
  "dispatch --next --force: explains that force is id-directed"

out=$(bash "$SCRIPT" reap 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "reap with no id: ok:false"
assert_contains "$(jqf "$out" .display)" "usage" "reap with no id: usage message"

out=$(bash "$SCRIPT" reap SH-1 junk 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "reap with a trailing token: ok:false"
assert_contains "$(jqf "$out" .display)" "usage" "reap with a trailing token: usage message"

out=$(bash "$SCRIPT" notify SH-1 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "notify with no message: ok:false"
assert_contains "$(jqf "$out" .display)" "usage" "notify with no message: usage message"

out=$(bash "$SCRIPT" bogus-subcommand 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unknown subcommand: ok:false"

# The usage string is the only discovery surface a caller has when it guesses
# wrong, so it must name every verb the router actually accepts. Pins the two
# from drifting apart as verbs are added.
usage=$(jqf "$(bash "$SCRIPT" bogus-subcommand 2>&1)" .display)
for verb in $(router_verbs "$SCRIPT"); do
  assert_contains "$usage" "$verb" "usage: names the \`$verb\` verb"
done

finish
