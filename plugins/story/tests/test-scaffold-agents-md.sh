#!/usr/bin/env bash
# `story.sh scaffold-agents-md` owns the safe AGENTS.md setup seam. In
# particular, `story project new` already writes the canonical file: setup must
# recognise that exact content rather than append a duplicate Storyhook block.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo AGT)
cd "$repo"

BEGIN='<!-- BEGIN STORYHOOK -->'
END='<!-- END STORYHOOK -->'

# Current project creation writes the canonical AGENTS.md. If that behavior
# ever changes, seed the exact canonical result so this test still isolates the
# helper's no-duplicate contract; project-new coverage belongs to the CLI suite.
if [ ! -f AGENTS.md ]; then
  story scaffold agents-md >AGENTS.md
fi
cp AGENTS.md /tmp/story-agents-before.$$
_TMP_REPOS+=("/tmp/story-agents-before.$$")

out=$(bash "$SCRIPT" scaffold-agents-md 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "exact scaffold: ok"
assert_eq "$(jqf "$out" .action)" "unchanged" "exact scaffold: no duplicate append"
cmp -s AGENTS.md /tmp/story-agents-before.$$ \
  || fail_test "exact scaffold: helper changed project-new's canonical AGENTS.md"
assert_eq "$(grep -cF "$BEGIN" AGENTS.md 2>/dev/null || true)" "0" \
  "exact scaffold: no redundant sentinel copy"

# A user-authored file gets one isolated Storyhook block, with its text intact.
printf 'user prefix\n\nuser suffix\n' >AGENTS.md
out=$(bash "$SCRIPT" scaffold-agents-md 2>&1)
assert_eq "$(jqf "$out" .action)" "appended" "user file: appended"
grep -qF 'user prefix' AGENTS.md || fail_test "user file: prefix was damaged"
grep -qF 'user suffix' AGENTS.md || fail_test "user file: suffix was damaged"
assert_eq "$(grep -cF "$BEGIN" AGENTS.md)" "1" "user file: one begin sentinel"
assert_eq "$(grep -cF "$END" AGENTS.md)" "1" "user file: one end sentinel"

# Refresh replaces only the delimited block.
printf 'before\n%s\nSTALE\n%s\nafter\n' "$BEGIN" "$END" >AGENTS.md
out=$(bash "$SCRIPT" scaffold-agents-md 2>&1)
assert_eq "$(jqf "$out" .action)" "replaced" "refresh: replaced"
grep -qF 'before' AGENTS.md || fail_test "refresh: prefix was damaged"
grep -qF 'after' AGENTS.md || fail_test "refresh: suffix was damaged"
grep -qF 'STALE' AGENTS.md && fail_test "refresh: stale block survived"

# A malformed sentinel is ambiguous user data: refuse and leave it byte-exact.
printf 'keep me\n%s\nunterminated\n' "$BEGIN" >AGENTS.md
cp AGENTS.md /tmp/story-agents-malformed.$$
_TMP_REPOS+=("/tmp/story-agents-malformed.$$")
out=$(bash "$SCRIPT" scaffold-agents-md 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "malformed: refused"
assert_contains "$(jqf "$out" .display)" "malformed" "malformed: precise reason"
cmp -s AGENTS.md /tmp/story-agents-malformed.$$ \
  || fail_test "malformed: helper rewrote the file despite refusing"

printf 'keep me\n%s\nwrong order\n%s\n' "$END" "$BEGIN" >AGENTS.md
cp AGENTS.md /tmp/story-agents-reversed.$$
_TMP_REPOS+=("/tmp/story-agents-reversed.$$")
out=$(bash "$SCRIPT" scaffold-agents-md 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "reversed sentinels: refused"
cmp -s AGENTS.md /tmp/story-agents-reversed.$$ \
  || fail_test "reversed sentinels: helper rewrote the file despite refusing"

# Preview is provider-neutral and side-effect free.
rm -f AGENTS.md
out=$(STORY_DRY_RUN=1 bash "$SCRIPT" scaffold-agents-md 2>&1)
assert_eq "$(jqf "$out" .dry_run)" "true" "dry run: flagged"
assert_eq "$(jqf "$out" .action)" "created" "dry run: previews create"
[ ! -e AGENTS.md ] || fail_test "dry run: wrote AGENTS.md"

finish
