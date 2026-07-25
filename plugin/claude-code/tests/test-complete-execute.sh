#!/usr/bin/env bash
# `story.sh complete execute <id>` — the destructive half. Every assertion
# about a survivor is made against git itself (show-ref / on-disk), never
# against the emitted JSON alone: the JSON reporting a branch as "preserved"
# is worth nothing if it was deleted anyway.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- dry run previews without touching anything ---
id=$(new_story "$repo" "Dry run me")
w=$(mk_dispatched "$repo" "$id")
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" complete execute "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry: ok"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry: flagged"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "git worktree remove" "dry: previews the removal"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "story move $id done" "dry: previews the close"
[ -d "$repo/.claude/worktrees/$w" ] || fail_test "dry: worktree was actually removed"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "todo" \
  "dry: story state untouched"

# --- the happy path really removes both, and really closes the story ---
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "exec: ok"
assert_eq "$(jqf "$out" .closed)" "true" "exec: closed"
assert_eq "$(jqf "$out" .closed_as)" "done" "exec: closed into the resolved CLOSED state"
assert_eq "$(jqf "$out" '.removed.worktrees|length')" "1" "exec: one worktree removed"
assert_eq "$(jqf "$out" '.removed.branches|length')" "1" "exec: one branch removed"
[ -d "$repo/.claude/worktrees/$w" ] && fail_test "exec: worktree still on disk"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$w") \
  && fail_test "exec: branch still in git"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.superstate')" "CLOSED" \
  "exec: story is closed per the real CLI"

# --- GUARD: an unmerged branch survives, and its commit survives with it ---
un=$(new_story "$repo" "Unmerged")
wun=$(mk_dispatched "$repo" "$un")
(cd "$repo/.claude/worktrees/$wun" && echo x >x && git add x && git commit -qm work) >/dev/null 2>&1
unsha=$(cd "$repo" && git rev-parse "refs/heads/worktree-$wun")
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$un" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "unmerged: ok (cleanup still succeeds)"
assert_eq "$(jqf "$out" '.removed.branches|length')" "0" "unmerged: no branch removed"
assert_contains "$(jqf "$out" '.skipped|join(" ")')" "unmerged" "unmerged: reported as preserved"
assert_eq "$(cd "$repo" && git rev-parse "refs/heads/worktree-$wun")" "$unsha" \
  "unmerged: branch survives at the same commit — the work is not lost"

# --- GUARD: a dirty worktree survives ---
dy=$(new_story "$repo" "Dirty")
wdy=$(mk_dispatched "$repo" "$dy")
echo scratch >"$repo/.claude/worktrees/$wdy/scratch.txt"
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$dy" 2>&1)
assert_eq "$(jqf "$out" '.removed.worktrees|length')" "0" "dirty: worktree not removed"
[ -d "$repo/.claude/worktrees/$wdy" ] || fail_test "dirty: worktree was removed despite being dirty"
[ -f "$repo/.claude/worktrees/$wdy/scratch.txt" ] || fail_test "dirty: uncommitted file was destroyed"

# --- GUARD: a locked worktree survives and is never unlocked ---
lk=$(new_story "$repo" "Locked")
wlk=$(mk_dispatched "$repo" "$lk")
(cd "$repo" && git worktree lock ".claude/worktrees/$wlk") >/dev/null 2>&1
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$lk" 2>&1)
assert_contains "$(jqf "$out" '.skipped|join(" ")')" "locked" "locked: reported as preserved"
[ -d "$repo/.claude/worktrees/$wlk" ] || fail_test "locked: worktree was removed"
assert_contains "$(cd "$repo" && git worktree list --porcelain)" "locked" "locked: still locked afterwards"

# --- GUARD: main is never touched by any of the above ---
(cd "$repo" && git show-ref --verify --quiet refs/heads/main) \
  || fail_test "main branch was deleted"

# --- --no-close cleans up but leaves the story open ---
nc=$(new_story "$repo" "No close")
wnc=$(mk_dispatched "$repo" "$nc")
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$nc" --no-close 2>&1)
assert_eq "$(jqf "$out" .closed)" "false" "--no-close: not closed"
assert_eq "$(jqf "$out" '.removed.worktrees|length')" "1" "--no-close: still cleans up"
assert_eq "$(cd "$repo" && story show "$nc" --json | jq -r '.story.story.superstate')" "OPEN" \
  "--no-close: story really is still open"

# --- --no-clean closes but preserves the worktree ---
ncl=$(new_story "$repo" "No clean")
wncl=$(mk_dispatched "$repo" "$ncl")
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$ncl" --no-clean 2>&1)
assert_eq "$(jqf "$out" .closed)" "true" "--no-clean: closed"
assert_eq "$(jqf "$out" '.removed.worktrees|length')" "0" "--no-clean: nothing removed"
[ -d "$repo/.claude/worktrees/$wncl" ] || fail_test "--no-clean: worktree was removed anyway"

# --- STORY_DONE_STATE override ---
(cd "$repo" && story state add archived --super CLOSED >/dev/null 2>&1)
ov=$(new_story "$repo" "Override")
out=$(cd "$repo" && STORY_DONE_STATE=archived bash "$SCRIPT" complete execute "$ov" 2>&1)
assert_eq "$(jqf "$out" .closed_as)" "archived" "override: honours STORY_DONE_STATE"
assert_eq "$(cd "$repo" && story show "$ov" --json | jq -r '.story.story.state')" "archived" \
  "override: story really moved into the overridden state"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$id" --bogus 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "exec: unknown flag is rejected"
assert_contains "$(jqf "$out" .display)" "--no-close" "exec: usage names the real flags"

finish
