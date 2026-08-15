#!/usr/bin/env bash
# `story.sh handoff [--since <duration>]` (SH-308) — wraps `story handoff` and
# `story summary --json`. `--since` is passed through ONLY when given; the
# real fix here is that this script has NO hand-copied default to state —
# `skills/story-handoff/SKILL.md` said `2h` after the CLI's real default had
# been `24h` for a full release, because the fix that corrected the CLI's own
# help text (04ac259) never touched the skill. Passing --since through
# unconditionally, instead of restating a number, is what makes that
# particular drift structurally impossible here.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Handoff me")

out=$(cd "$repo" && bash "$SCRIPT" handoff --since 1d 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "handoff: ok"
assert_contains "$(jqf "$out" .display)" "$id" "handoff: display carries the real story"
assert_contains "$(jqf "$out" .display)" "Session Handoff" "handoff: display is the real handoff document"
assert_eq "$(jqf "$out" .summary.summary.total_open)" "1" "handoff: embeds a real summary snapshot"

# --- no --since given: no --since is passed to the CLI at all ---
out=$(cd "$repo" && bash "$SCRIPT" handoff 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "handoff: works with no --since"

# --- read-only ---
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "todo" \
  "handoff: does not mutate anything"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" handoff --bogus 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "handoff: rejects an unknown flag"

finish
