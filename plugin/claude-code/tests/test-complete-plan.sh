#!/usr/bin/env bash
# `story.sh complete plan <id>` — the read-only preview the skill shows
# before asking for confirmation. Pins the classification matrix and proves
# the preview mutates nothing.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- everything actionable: open story, clean worktree, merged branch ---
id=$(new_story "$repo" "Cleanup me")
w=$(mk_dispatched "$repo" "$id")
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "plan: ok"
assert_eq "$(jqf "$out" .plan.worktree.status)" "removable" "plan: clean worktree is removable"
assert_eq "$(jqf "$out" .plan.branch.status)" "deletable" "plan: merged branch is deletable"
assert_eq "$(jqf "$out" .plan.close.to)" "done" "plan: resolves the CLOSED state from states.toml"
assert_eq "$(jqf "$out" .actions_count)" "3" "plan: counts close + worktree + branch"

# The preview really is read-only.
[ -d "$repo/.claude/worktrees/$w" ] || fail_test "plan: worktree was removed by a PLAN run"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$w") \
  || fail_test "plan: branch was deleted by a PLAN run"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "todo" \
  "plan: story state untouched by a PLAN run"

# --- unmerged branch is preserved ---
un=$(new_story "$repo" "Unmerged")
wun=$(mk_dispatched "$repo" "$un")
(cd "$repo/.claude/worktrees/$wun" && echo x >x && git add x && git commit -qm work) >/dev/null 2>&1
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$un" 2>&1)
assert_eq "$(jqf "$out" .plan.branch.status)" "unmerged" "plan: branch with its own commit is unmerged"
assert_eq "$(jqf "$out" .actions_count)" "2" "plan: unmerged branch is not counted as an action"
assert_contains "$(jqf "$out" .display)" "PRESERVED" "plan: display flags the preserved branch"

# --- dirty worktree is preserved ---
dy=$(new_story "$repo" "Dirty")
wdy=$(mk_dispatched "$repo" "$dy")
echo scratch >"$repo/.claude/worktrees/$wdy/scratch.txt"
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$dy" 2>&1)
assert_eq "$(jqf "$out" .plan.worktree.status)" "dirty" "plan: worktree with uncommitted changes is dirty"

# --- locked worktree is preserved (Claude Code's own worktrees are locked) ---
lk=$(new_story "$repo" "Locked")
wlk=$(mk_dispatched "$repo" "$lk")
(cd "$repo" && git worktree lock ".claude/worktrees/$wlk") >/dev/null 2>&1
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$lk" 2>&1)
assert_eq "$(jqf "$out" .plan.worktree.status)" "locked" "plan: locked worktree is reported locked"

# --- the current worktree is never removable ---
out=$(cd "$repo/.claude/worktrees/$wdy" && bash "$SCRIPT" complete plan "$dy" 2>&1)
assert_eq "$(jqf "$out" .plan.worktree.status)" "current" \
  "plan: run from inside the target worktree, it classifies as current"

# --- nothing dispatched at all ---
bare=$(new_story "$repo" "Never dispatched")
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$bare" 2>&1)
assert_eq "$(jqf "$out" .plan.worktree.status)" "missing" "plan: absent worktree is missing"
assert_eq "$(jqf "$out" .plan.branch.status)" "missing" "plan: absent branch is missing"
assert_eq "$(jqf "$out" .actions_count)" "1" "plan: only the close remains as an action"

# --- an already-closed story proposes no close ---
(cd "$repo" && story move "$bare" done >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$bare" 2>&1)
assert_eq "$(jqf "$out" .plan.close)" "null" "plan: closed story proposes no close"
assert_eq "$(jqf "$out" .actions_count)" "0" "plan: nothing left to do"
assert_contains "$(jqf "$out" .display)" "already closed" "plan: display says so"

# --- the default branch is never a target ---
assert_contains "$(jqf "$out" .default_branch)" "main" "plan: reports the default branch"
out_all=$(cd "$repo" && bash "$SCRIPT" complete plan "$id" 2>&1)
assert_eq "$(jqf "$out_all" '.plan.branch.name | test("^worktree-")')" "true" \
  "plan: only ever targets a worktree-* branch, never main"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" complete plan "TST-9999" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "plan: unknown story is ok:false"
out=$(cd "$repo" && bash "$SCRIPT" complete plan 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "plan: missing id is ok:false"
out=$(cd "$repo" && bash "$SCRIPT" complete plan "bad id!" 2>&1)
assert_contains "$(jqf "$out" .display)" "alphanumeric" "plan: invalid id is rejected"
out=$(cd "$repo" && bash "$SCRIPT" complete bogus "$id" 2>&1)
assert_contains "$(jqf "$out" .display)" "plan|execute" "plan: unknown sub-verb names the valid ones"

finish
