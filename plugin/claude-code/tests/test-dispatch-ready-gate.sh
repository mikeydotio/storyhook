#!/usr/bin/env bash
# Issue #40's core ask: /story do must refuse a story that isn't ready,
# reporting a specific reason, before any side effect (no worktree, no
# tmux window, no claim, no state change). Exercises the ready-gate
# against the REAL story CLI (this repo builds it) — a fake would risk
# drifting from is_ready()'s actual open/awaiting/obviated-by/blocked-by
# semantics, the exact thing this gate exists to track faithfully.
# STORY_DRY_RUN=1 is enough here: every case under test refuses BEFORE the
# dry-run/real-run branch point, so no tmux is needed.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- awaiting ---
awaiting_id=$(new_story "$repo" "Awaiting story")
(cd "$repo" && story block "$awaiting_id" "waiting on design" >/dev/null)
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$awaiting_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "awaiting: ok:false"
assert_contains "$(jqf "$out" .display)" "awaiting" "awaiting: reason names awaiting"
assert_contains "$(jqf "$out" .display)" "waiting on design" "awaiting: reason includes the awaiting text"

# --- closed (done) ---
done_id=$(new_story "$repo" "Done story")
(cd "$repo" && story move "$done_id" "done" >/dev/null)
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$done_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "closed: ok:false"
assert_contains "$(jqf "$out" .display)" "closed" "closed: reason names closed"

# --- blocked-by an open story ---
dep_id=$(new_story "$repo" "Dependency")
dependent_id=$(new_story "$repo" "Dependent")
(cd "$repo" && story relate "$dependent_id" blocked-by "$dep_id" >/dev/null)
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$dependent_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "blocked-by: ok:false"
assert_contains "$(jqf "$out" .display)" "blocked-by" "blocked-by: reason names the relationship"
assert_contains "$(jqf "$out" .display)" "$dep_id" "blocked-by: reason names the blocker id"

# --- sanity: a genuinely ready story is NOT refused by the gate ---
ready_id=$(new_story "$repo" "Ready story")
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$ready_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "ready: ok:true, gate does not over-refuse"

# No side effects from any of the refused attempts above: no worktree
# created, and every refused story's state is untouched.
[ -d "$repo/.claude/worktrees" ] && fail_test "ready-gate: a worktree was created despite every refusal"
awaiting_state=$(cd "$repo" && story show "$awaiting_id" --json | jq -r '.story.story.state')
assert_eq "$awaiting_state" "todo" "awaiting: state untouched (still todo) after a refused dispatch"

finish
