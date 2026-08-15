#!/usr/bin/env bash
# `story.sh triage` (SH-308) — the four reads skills/story-triage/SKILL.md
# used to run separately (list, --stale, --blocked, and a cycle check the
# skill used to hand the model as "eyeball `story graph`'s output ... this
# is a manual check"), pre-classified into findings[]. Read-only: the
# skill's own resolution commands (prioritize/label/block/…) stay direct
# `story` calls, unchanged, so this file asserts nothing about them.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- a fresh project answers ok:true ---
out=$(cd "$repo" && bash "$SCRIPT" triage 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "fresh project: ok"

# --- unprioritized ---
unp=$(new_story "$repo" "No priority")
out=$(cd "$repo" && bash "$SCRIPT" triage 2>&1)
assert_contains "$(jqf "$out" '[.findings[]|select(.category=="unprioritized")|.id]|join(",")')" "$unp" \
  "unprioritized: flagged"
assert_eq "$(jqf "$out" '.counts.unprioritized >= 1')" "true" "unprioritized: counted"

# --- orphan (no relationships at all) ---
orp=$(new_story "$repo" "No relationships")
(cd "$repo" && story prioritize "$orp" low >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" triage 2>&1)
assert_contains "$(jqf "$out" '[.findings[]|select(.category=="orphan")|.id]|join(",")')" "$orp" \
  "orphan: flagged"

# --- blocked ---
blk=$(new_story "$repo" "Blocked story")
(cd "$repo" && story block "$blk" "waiting on something" >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" triage 2>&1)
assert_contains "$(jqf "$out" '[.findings[]|select(.category=="blocked")|.id]|join(",")')" "$blk" \
  "blocked: flagged"
assert_contains "$(jqf "$out" '.findings[]|select(.id=="'"$blk"'")|.detail')" "waiting on something" \
  "blocked: carries the real reason"

# --- a genuine blocking CYCLE is detected, not just eyeballed ---
a=$(new_story "$repo" "Cycle A")
b=$(new_story "$repo" "Cycle B")
c=$(new_story "$repo" "Cycle C")
(cd "$repo" && story relate "$a" blocked-by "$b" >/dev/null 2>&1)
(cd "$repo" && story relate "$b" blocked-by "$c" >/dev/null 2>&1)
(cd "$repo" && story relate "$c" blocked-by "$a" >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" triage 2>&1)
cyc=$(jqf "$out" '[.findings[]|select(.category=="cycle")|.id]|sort|join(",")')
assert_eq "$cyc" "$(printf '%s\n%s\n%s' "$a" "$b" "$c" | sort | paste -sd, -)" \
  "cycle: all three cycle members flagged, nothing more"

# --- a non-cyclic chain is NOT flagged as a cycle (no false positive) ---
d=$(new_story "$repo" "Chain D")
e=$(new_story "$repo" "Chain E")
(cd "$repo" && story relate "$d" blocked-by "$e" >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" triage 2>&1)
case "$(jqf "$out" '[.findings[]|select(.category=="cycle")|.id]|join(",")')" in
  *"$d"*|*"$e"*) fail_test "chain: a plain dependency chain was flagged as a cycle" ;;
esac

# --- STORY_STALE_THRESHOLD reaches the real `story list --stale` call rather
# than a hard-coded window -- every story here is seconds old, so nothing IS
# stale yet; this proves the flag is accepted and plumbed through (ok:true,
# a real stale count), not that staleness fires (which needs an old story,
# not reproducible without manipulating the clock) ---
out=$(cd "$repo" && STORY_STALE_THRESHOLD=1h bash "$SCRIPT" triage 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "stale threshold: still ok with a custom window"
assert_eq "$(jqf "$out" '.counts.stale')" "0" "stale threshold: nothing is old enough yet"
out=$(cd "$repo" && STORY_STALE_THRESHOLD='not-a-duration' bash "$SCRIPT" triage 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "stale threshold: a malformed duration is rejected by the real CLI, not silently accepted"

# --- read-only: nothing about the real project changed ---
assert_eq "$(cd "$repo" && story show "$unp" --json | jq -r '.story.story.priority')" "none" \
  "read-only: triage itself never mutates a story"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" triage extra 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "triage: rejects arguments"

finish
