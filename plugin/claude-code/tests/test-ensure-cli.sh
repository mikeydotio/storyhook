#!/usr/bin/env bash
# `story.sh ensure-cli` (SH-308) — the ONE verb that must answer even when
# `story` itself is missing, replacing six copies of the same "is the CLI
# installed" preamble that used to live in prose. Always ok:true: absence is a
# real fact this reports, not a failure of the check.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- the CLI is present (the harness's own PATH has it) ---
out=$(cd "$repo" && bash "$SCRIPT" ensure-cli 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "present: ok"
assert_eq "$(jqf "$out" .installed)" "true" "present: installed:true"
assert_contains "$(jqf "$out" .version)" "story" "present: version names the binary"
assert_contains "$(jqf "$out" .display)" "installed" "present: display says so"

# --- the CLI is absent: a minimal PATH with no `story` on it. PATH ONLY --
# never `env -i`, which would also strip the isolation vars lib.sh exported
# for this whole process (STORYHOOK_DATA_DIR, HOME, XDG_*) and risk a stray
# write to the real store. ---
out=$(cd "$repo" && PATH="/usr/bin:/bin" bash "$SCRIPT" ensure-cli 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "absent: still ok:true -- a successful check, not a failure"
assert_eq "$(jqf "$out" .installed)" "false" "absent: installed:false"
assert_eq "$(jqf "$out" .version)" "" "absent: no version to report"
assert_contains "$(jqf "$out" .display)" "not installed" "absent: display says so plainly"

# --- works from outside any project (no repo, no checkout) ---
scratch=$(mktemp -d /tmp/story-test-scratch.XXXXXX)
_TMP_REPOS+=("$scratch")
out=$(cd "$scratch" && bash "$SCRIPT" ensure-cli 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "no-project: still answers -- this verb never resolves a project"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" ensure-cli extra 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "ensure-cli: rejects extra arguments"

finish
