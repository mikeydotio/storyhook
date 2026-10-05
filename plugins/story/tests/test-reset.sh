#!/usr/bin/env bash
# `story.sh reset <id> [--force]` delegates to the native `story reset`
# (SH-886): one contract for every reset. It closes the story's window,
# discards its worktree and local branch -- dirty, locked or unpushed --
# clears its awaiting reason and returns the story to todo. Once the story is
# reserved it never refuses; what it cannot prove the story owns is left and
# named in the story's comment, with the command that recovers the deleted
# branch. `--force` is accepted and changes nothing. Stop Now's engine mode is
# covered by test-engine-reset.sh.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
slug=$(slug_for "$repo")

state_of() { (cd "$repo" && story show "$1" --json | jq -r '.story.story.state'); }
comments_of() { (cd "$repo" && story show "$1" --json | jq -r '[.story.story.comments[].text] | join("|")'); }
claim_it() { (cd "$repo" && story claim "$1" --no-comment --json >/dev/null 2>&1); }
wt_exists() { [ -d "$repo/.claude/worktrees/$1" ]; }
br_exists() { (cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$1"); }

# commit_in <wname> — one local commit inside a dispatched worktree: work that
# exists on no remote, which reset now discards and records how to recover.
commit_in() {
  (cd "$repo/.claude/worktrees/$1" && echo x >x && git add x && git commit -qm work) >/dev/null 2>&1
}

# --- dirty, unpushed work is discarded without --force ---------------------
hp=$(new_story "$repo" "Reset me")
whp=$(mk_dispatched "$repo" "$hp")
claim_it "$hp"
commit_in "$whp"
echo scratch >"$repo/.claude/worktrees/$whp/scratch.txt"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$hp" 2>&1)
assert_ok "$out" "true" "reset: ok"
assert_eq "$(jqf "$out" .state)" "todo" "reset: the answer reports todo"
assert_eq "$(state_of "$hp")" "todo" "reset: the REAL story moved"
assert_contains "$(jqf "$out" .display)" "returned to" "reset: the display says where it went"
assert_eq "$(jqf "$out" .completed)" "true" "reset: the answer says it finished"
assert_eq "$(jqf "$out" .removed.worktree)" "true" "reset: the answer reports the worktree removed"
assert_eq "$(jqf "$out" .removed.branch)" "true" "reset: the answer reports the branch removed"
assert_contains "$(jqf "$out" .recovery.branch)" "worktree-$whp" "reset: the answer carries the recovery record"
wt_exists "$whp" && fail_test "reset: worktree still on disk"
br_exists "$whp" && fail_test "reset: branch still in git"
assert_contains "$(comments_of "$hp")" "git branch worktree-$whp" \
  "reset: the comment names how to recover the deleted branch"

# --- --force is accepted and changes nothing; --comment adds a comment ------
fo=$(new_story "$repo" "Forced reset")
wfo=$(mk_dispatched "$repo" "$fo")
claim_it "$fo"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$fo" --force --comment "Restart from scratch" 2>&1)
assert_ok "$out" "true" "force: ok"
assert_eq "$(state_of "$fo")" "todo" "force: the story moved"
assert_contains "$(comments_of "$fo")" "Restart from scratch" "force: --comment was recorded"
wt_exists "$wfo" && fail_test "force: worktree still on disk"

# --- what reset cannot remove is reported, never refused --------------------
# The caller's own worktree is residue: the story is still released.
own=$(new_story "$repo" "Reset from inside")
wown=$(mk_dispatched "$repo" "$own")
claim_it "$own"
out=$(cd "$repo/.claude/worktrees/$wown" && bash "$SCRIPT" --project "$slug" reset "$own" 2>&1)
assert_ok "$out" "true" "own: ok"
assert_eq "$(state_of "$own")" "todo" "own: the story moved"
wt_exists "$wown" || fail_test "own: the caller's worktree was removed"
assert_eq "$(jqf "$out" .removed.worktree)" "false" "own: the worktree is not reported removed"
assert_contains "$(jqf "$out" '[.residue[].resource] | join(",")')" "worktree " "own: the answer names the worktree left in place"
assert_contains "$(jqf "$out" .display)" "Left in place" "own: the display says what was left"

# --- a dry run previews the delegated command and changes nothing ----------
dr=$(new_story "$repo" "Dry run")
wdr=$(mk_dispatched "$repo" "$dr")
claim_it "$dr"
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" --project "$slug" reset "$dr" 2>&1)
assert_ok "$out" "true" "dry: ok"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry: says it is a dry run"
assert_contains "$(jqf "$out" .display)" "story reset $dr" "dry: names the delegated command"
wt_exists "$wdr" || fail_test "dry: worktree was actually removed"
assert_eq "$(state_of "$dr")" "in-progress" "dry: the story did NOT move"

# --- requests that are not an open-story reset still refuse ----------------
cl=$(new_story "$repo" "Closed")
(cd "$repo" && story move "$cl" done --json >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$cl" 2>&1)
assert_ok "$out" "false" "closed: a closed story is not reset"
assert_contains "$(jqf "$out" .display)" "closed" "closed: the refusal says why"
out=$(cd "$repo" && bash "$SCRIPT" reset 2>&1)
assert_ok "$out" "false" "reset: missing id is ok:false"
assert_contains "$(jqf "$out" .display)" "usage:" "reset: missing id shows the usage line"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$hp" junk 2>&1)
assert_ok "$out" "false" "reset: a word that lands nowhere is refused"

finish
