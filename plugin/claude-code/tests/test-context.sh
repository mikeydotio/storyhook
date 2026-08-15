#!/usr/bin/env bash
# `story.sh context [--full]` (SH-308) — wraps `story load-context` (which
# already IS the comprehensive overview the skill used to assemble by hand
# from two separate calls) and, under `--full`, the three deep-dive reads.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Context me")

out=$(cd "$repo" && bash "$SCRIPT" context 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "context: ok"
assert_eq "$(jqf "$out" .full)" "false" "context: full defaults false"
assert_contains "$(jqf "$out" .display)" "$id" "context: display carries the real story"

# --- --full appends the three deep-dive sections, none present by default ---
out=$(cd "$repo" && bash "$SCRIPT" context --full 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "full: ok"
assert_eq "$(jqf "$out" .full)" "true" "full: reports itself"
assert_contains "$(jqf "$out" .display)" "Critical path" "full: appends critical-path"
assert_contains "$(jqf "$out" .display)" "Blocked stories" "full: appends blocked"
assert_contains "$(jqf "$out" .display)" "Stale" "full: appends stale"

out=$(cd "$repo" && bash "$SCRIPT" context 2>&1)
case "$(jqf "$out" .display)" in
  *"Critical path"*) fail_test "context: default run should NOT include the --full sections" ;;
esac

# --- STORY_STALE_THRESHOLD overrides the un-hardcoded staleness window ---
out=$(cd "$repo" && STORY_STALE_THRESHOLD=1d bash "$SCRIPT" context --full 2>&1)
assert_contains "$(jqf "$out" .display)" "Stale (1d+)" "full: honours STORY_STALE_THRESHOLD"

# --- read-only ---
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "todo" \
  "context: does not mutate anything"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" context --bogus 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "context: rejects an unknown flag"

finish
