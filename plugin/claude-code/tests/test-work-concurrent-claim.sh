#!/usr/bin/env bash
# SH-346: a safety net beside the deterministic conflict/retry/exhausted
# fixtures above -- genuine concurrency, no fake in the loop. N `story.sh
# work` (no id) calls fired at once against the same pool of ready stories
# must never report the same story to two of them, and the real store must
# show exactly the claimed stories moved. This test CAN pass by luck on the
# unguarded pre-fix code (the daemon serializes the underlying writes either
# way) -- it is not the red/green gate for the fix; the deterministic fakes
# above are. It exists because a scheduling-dependent invariant is worth
# checking under real scheduling too, not just under a scripted double.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
ids=()
for i in 1 2 3 4; do
  sid=$(new_story "$repo" "Concurrent claim target $i")
  (cd "$repo" && story prioritize "$sid" medium >/dev/null 2>&1)
  ids+=("$sid")
done

outdir=$(mktemp -d /tmp/story-test-concurrent.XXXXXX)
_TMP_REPOS+=("$outdir")

pids=()
for i in 1 2 3 4; do
  (cd "$repo" && bash "$SCRIPT" work >"$outdir/out-$i.json" 2>&1) &
  pids+=("$!")
done
for pid in "${pids[@]}"; do
  wait "$pid"
done

claimed=()
for i in 1 2 3 4; do
  out=$(cat "$outdir/out-$i.json")
  if [ "$(jqf "$out" .ok)" = "true" ] && [ "$(jqf "$out" .picked)" = "true" ]; then
    claimed+=("$(jqf "$out" .id)")
  fi
done

assert_eq "${#claimed[@]}" "4" "concurrent: all four workers claimed a story (4 ready stories, 4 workers)"
unique_count=$(printf '%s\n' "${claimed[@]}" | sort -u | wc -l | tr -d ' ')
assert_eq "$unique_count" "4" "concurrent: no two workers claimed the SAME story"

for sid in "${ids[@]}"; do
  real_state=$(cd "$repo" && story show "$sid" --json | jq -r '.story.story.state')
  assert_eq "$real_state" "in-progress" "concurrent: $sid ended up claimed exactly once, for real"
done

finish
