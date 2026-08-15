#!/usr/bin/env bash
# `story.sh scaffold-claude-md [--path <file>]` (SH-308) — sentinel-delimited
# insert-or-replace of `story scaffold claude-md`'s output, replacing
# skills/story-setup/SKILL.md's hand-rolled text surgery. Every assertion
# checks the REAL file on disk, never just the emitted JSON.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
cd "$repo"

BEGIN='<!-- BEGIN STORYHOOK -->'
END='<!-- END STORYHOOK -->'

# --- no CLAUDE.md at all: creates one ---
out=$(bash "$SCRIPT" scaffold-claude-md 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "create: ok"
assert_eq "$(jqf "$out" .action)" "created" "create: reports created"
[ -f CLAUDE.md ] || fail_test "create: CLAUDE.md was not written"
grep -qF "$BEGIN" CLAUDE.md || fail_test "create: begin sentinel missing"
grep -qF "$END" CLAUDE.md || fail_test "create: end sentinel missing"
grep -qF "storyhook" CLAUDE.md || fail_test "create: real scaffold content missing"

# --- CLAUDE.md exists WITHOUT the sentinel: appends, preserving what's there ---
printf '# My project\n\nSome existing notes.\n' > CLAUDE.md
out=$(bash "$SCRIPT" scaffold-claude-md 2>&1)
assert_eq "$(jqf "$out" .action)" "appended" "append: reports appended"
grep -qF "Some existing notes." CLAUDE.md || fail_test "append: pre-existing content was destroyed"
grep -qF "$BEGIN" CLAUDE.md || fail_test "append: begin sentinel missing"
assert_eq "$(grep -cF "$BEGIN" CLAUDE.md)" "1" "append: exactly one sentinel pair"

# --- CLAUDE.md already has the block: REPLACES it, byte-preserving the rest ---
printf 'before\n%s\nSTALE OLD CONTENT LINE 1\nSTALE OLD CONTENT LINE 2\n%s\nafter\n' "$BEGIN" "$END" > CLAUDE.md
out=$(bash "$SCRIPT" scaffold-claude-md 2>&1)
assert_eq "$(jqf "$out" .action)" "replaced" "replace: reports replaced"
grep -qF "before" CLAUDE.md || fail_test "replace: content BEFORE the block was destroyed"
grep -qF "after" CLAUDE.md || fail_test "replace: content AFTER the block was destroyed"
grep -qF "STALE OLD CONTENT" CLAUDE.md && fail_test "replace: the stale block content survived"
assert_eq "$(grep -cF "$BEGIN" CLAUDE.md)" "1" "replace: exactly one sentinel pair after replace"
assert_eq "$(grep -cF "$END" CLAUDE.md)" "1" "replace: exactly one end sentinel after replace"

# --- idempotent: running it twice in a row converges, no drift ---
first=$(cat CLAUDE.md)
bash "$SCRIPT" scaffold-claude-md >/dev/null 2>&1
second=$(cat CLAUDE.md)
assert_eq "$second" "$first" "idempotent: a second run changes nothing further"

# --- --path targets a different file, and never touches CLAUDE.md ---
rm -f CLAUDE.md
out=$(bash "$SCRIPT" scaffold-claude-md --path AGENTS.md 2>&1)
assert_eq "$(jqf "$out" .path)" "AGENTS.md" "path: reports the override"
[ -f AGENTS.md ] || fail_test "path: AGENTS.md was not written"
[ -f CLAUDE.md ] && fail_test "path: CLAUDE.md was created even though --path named something else"

# --- dry run previews without touching anything ---
rm -f CLAUDE.md AGENTS.md
out=$(STORY_DRY_RUN=1 bash "$SCRIPT" scaffold-claude-md 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry: ok"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry: flagged"
assert_contains "$(jqf "$out" .display)" "would create" "dry: correct present-tense verb, not \"would created\""
[ -f CLAUDE.md ] && fail_test "dry: CLAUDE.md was actually written"

# --- errors ---
out=$(bash "$SCRIPT" scaffold-claude-md --path 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "path with no value: rejected"
out=$(bash "$SCRIPT" scaffold-claude-md --bogus 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unknown flag: rejected"

finish
