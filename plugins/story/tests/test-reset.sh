#!/usr/bin/env bash
# `story.sh reset <id> [--force]` (SH-484) — `unclaim`, then delete the
# worktree AND the branch. The verb for a story inadvertently abandoned (crash,
# reboot) where restarting beats inheriting.
#
# Every refusal below answers one question: "am I about to destroy something
# that is not recoverable elsewhere?" That is why `--force` is ONE flag rather
# than three, and why the two cases it does NOT cover are the two where the
# answer is about something other than recoverability -- a protected branch (a
# repository-level statement, not a scratch artifact) and self-termination
# (destroying the caller's own ground mid-command).
#
# Note what `unmerged` does NOT do here, unlike in reap: reset deletes an
# unmerged branch on purpose. Merged-ness is the wrong question -- a branch
# pushed to origin is fully recoverable without being merged anywhere, and a
# branch that only ever lived on this disk is not recoverable at all even
# though `git branch -d` would call it unmerged either way.
source "$(dirname "$0")/lib.sh"

export PATH="$TESTS_DIR/fakes:$PATH"
export FAKE_TMUX_STATE
FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")

repo=$(mk_story_repo)
slug=$(slug_for "$repo")

state_of() { (cd "$repo" && story show "$1" --json | jq -r '.story.story.state'); }
claim_it() { (cd "$repo" && story claim "$1" --no-comment --json >/dev/null 2>&1); }
wt_exists() { [ -d "$repo/.claude/worktrees/$1" ]; }
br_exists() { (cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$1"); }

# commit_in <wname> — one local commit inside a dispatched worktree, i.e. work
# that exists on no remote. This is what refusal 2 is about.
commit_in() {
  (cd "$repo/.claude/worktrees/$1" && echo x >x && git add x && git commit -qm work) >/dev/null 2>&1
}

# --- happy path: state released, worktree and branch gone, window closed ----
hp=$(new_story "$repo" "Reset me")
whp=$(mk_dispatched "$repo" "$hp")
claim_it "$hp"
out=$(cd "$repo" \
  && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="$(printf '%s\t1\t%%7' "$whp")" \
     bash "$SCRIPT" --project "$slug" reset "$hp" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "happy: ok"
assert_eq "$(jqf "$out" .unclaimed)" "true" "happy: the claim was released"
assert_eq "$(jqf "$out" .restored_to)" "todo" "happy: to the state it was claimed from"
assert_eq "$(jqf "$out" '.removed.worktree')" "true" "happy: worktree removed"
assert_eq "$(jqf "$out" '.removed.branch')" "true" "happy: branch removed"
assert_eq "$(jqf "$out" .closed_window)" "true" "happy: window closed"
assert_eq "$(state_of "$hp")" "todo" "happy: the REAL story really moved"
wt_exists "$whp" && fail_test "happy: worktree still on disk"
br_exists "$whp" && fail_test "happy: branch still in git"

# --- REFUSAL: a dirty worktree, and --force overrides ----------------------
dy=$(new_story "$repo" "Dirty")
wdy=$(mk_dispatched "$repo" "$dy")
claim_it "$dy"
echo scratch >"$repo/.claude/worktrees/$wdy/scratch.txt"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$dy" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dirty: ok:false"
assert_eq "$(jqf "$out" .reason)" "dirty-worktree" "dirty: reason names it"
wt_exists "$wdy" || fail_test "dirty: worktree was removed anyway"
[ -f "$repo/.claude/worktrees/$wdy/scratch.txt" ] || fail_test "dirty: uncommitted work destroyed"
assert_eq "$(state_of "$dy")" "in-progress" "dirty: NOTHING was mutated, the claim included"
assert_contains "$(jqf "$out" .display)" "--force" "dirty: names the override"

out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$dy" --force 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dirty --force: ok"
assert_eq "$(jqf "$out" .forced)" "true" "dirty --force: flagged as forced"
wt_exists "$wdy" && fail_test "dirty --force: worktree survived the override"

# --- REFUSAL: commits that exist on no remote, and --force overrides -------
up=$(new_story "$repo" "Unpushed")
wup=$(mk_dispatched "$repo" "$up")
commit_in "$wup"
claim_it "$up"
upsha=$(cd "$repo" && git rev-parse "refs/heads/worktree-$wup")
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$up" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unpushed: ok:false"
assert_eq "$(jqf "$out" .reason)" "unpushed-commits" "unpushed: reason names it"
assert_eq "$(jqf "$out" .unpushed)" "1" "unpushed: counts what would be destroyed"
assert_eq "$(cd "$repo" && git rev-parse "refs/heads/worktree-$wup")" "$upsha" \
  "unpushed: the branch survives at the same commit"
assert_eq "$(state_of "$up")" "in-progress" "unpushed: nothing was mutated"

out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$up" --force 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "unpushed --force: ok"
br_exists "$wup" && fail_test "unpushed --force: unmerged branch survived the override"

# --- a REMOTE-BACKED branch is not unpushed, even though it is unmerged -----
# The distinction reap cannot make: `branch_is_merged` says no, and reset
# deletes it anyway, because the remote already has every commit.
ps=$(new_story "$repo" "On the remote but unmerged")
wps=$(mk_dispatched "$repo" "$ps")
commit_in "$wps"
(cd "$repo/.claude/worktrees/$wps" && git push -q origin "HEAD:refs/heads/worktree-$wps") >/dev/null 2>&1
(cd "$repo" && git fetch -q origin "+refs/heads/worktree-$wps:refs/remotes/origin/worktree-$wps") >/dev/null 2>&1
claim_it "$ps"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$ps" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "remote-backed: no refusal — the remote has the commits"
assert_eq "$(jqf "$out" .unpushed)" "0" "remote-backed: nothing is unrecoverable"
assert_eq "$(jqf "$out" '.removed.branch')" "true" "remote-backed: the unmerged branch is deleted"
br_exists "$wps" && fail_test "remote-backed: local branch survived"

# --- REFUSAL: a locked worktree, and --force overrides ---------------------
lk=$(new_story "$repo" "Locked")
wlk=$(mk_dispatched "$repo" "$lk")
claim_it "$lk"
(cd "$repo" && git worktree lock ".claude/worktrees/$wlk") >/dev/null 2>&1
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$lk" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "locked: ok:false"
assert_eq "$(jqf "$out" .reason)" "locked-worktree" "locked: reason names it"
wt_exists "$wlk" || fail_test "locked: worktree was removed"
assert_contains "$(cd "$repo" && git worktree list --porcelain)" "locked" "locked: still locked"

out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$lk" --force 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "locked --force: ok"
wt_exists "$wlk" && fail_test "locked --force: worktree survived the override"

# --- REFUSAL: a protected branch, which --force does NOT override ----------
pr=$(new_story "$repo" "Protected")
wpr=$(mk_dispatched "$repo" "$pr")
claim_it "$pr"
for extra in "" "--force"; do
  # shellcheck disable=SC2086 # deliberate: "" must expand to no argument
  out=$(cd "$repo" && STORY_PROTECTED_BRANCHES="worktree-*" \
    bash "$SCRIPT" --project "$slug" reset "$pr" $extra 2>&1)
  assert_eq "$(jqf "$out" .ok)" "false" "protected${extra:+ $extra}: ok:false"
  assert_eq "$(jqf "$out" .reason)" "protected-branch" "protected${extra:+ $extra}: reason names it"
done
wt_exists "$wpr" || fail_test "protected: worktree was removed"
br_exists "$wpr" || fail_test "protected: branch was deleted"
assert_eq "$(state_of "$pr")" "in-progress" "protected: nothing was mutated"

# --- REFUSAL: self-termination, which --force does NOT override ------------
# The window the caller is sitting in.
sw=$(new_story "$repo" "Self window")
wsw=$(mk_dispatched "$repo" "$sw")
claim_it "$sw"
for extra in "" "--force"; do
  # shellcheck disable=SC2086
  out=$(cd "$repo" \
    && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="$(printf '%s\t1\t%%0' "$wsw")" \
       bash "$SCRIPT" --project "$slug" reset "$sw" $extra 2>&1)
  assert_eq "$(jqf "$out" .ok)" "false" "self-window${extra:+ $extra}: ok:false"
  assert_eq "$(jqf "$out" .reason)" "self-window" "self-window${extra:+ $extra}: reason names it"
done
wt_exists "$wsw" || fail_test "self-window: worktree was removed"
assert_eq "$(state_of "$sw")" "in-progress" "self-window: nothing was mutated"

# The worktree the caller's shell is standing in, with no tmux in play at all
# -- a different signal from the one above, and separately fatal.
cw=$(new_story "$repo" "Current worktree")
wcw=$(mk_dispatched "$repo" "$cw")
claim_it "$cw"
for extra in "" "--force"; do
  # shellcheck disable=SC2086
  out=$(cd "$repo/.claude/worktrees/$wcw" \
    && bash "$SCRIPT" --project "$slug" reset "$cw" $extra 2>&1)
  assert_eq "$(jqf "$out" .ok)" "false" "current-worktree${extra:+ $extra}: ok:false"
  assert_eq "$(jqf "$out" .reason)" "current-worktree" "current-worktree${extra:+ $extra}: reason names it"
done
wt_exists "$wcw" || fail_test "current-worktree: worktree was removed"
assert_eq "$(state_of "$cw")" "in-progress" "current-worktree: nothing was mutated"

# --- a lost CAS is REPORTED, and the teardown still runs -------------------
# A conflict proves the story is NOT in the active state, so no claim is held
# and the worktree is litter `reap` would refuse to touch (the story is open).
# Refusing here would leave a crashed lane with no sanctioned cleanup.
cf=$(new_story "$repo" "Never claimed")
wcf=$(mk_dispatched "$repo" "$cf")
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$cf" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "conflict: still ok — the teardown is the verb"
assert_eq "$(jqf "$out" .unclaimed)" "false" "conflict: no release happened"
assert_eq "$(jqf "$out" .unclaim_conflict)" "todo" "conflict: names the state actually found"
assert_contains "$(jqf "$out" .display)" "was not claimed" "conflict: display says so"
wt_exists "$wcf" && fail_test "conflict: worktree survived the teardown"

# --- nothing on disk is not an error ---------------------------------------
nd=$(new_story "$repo" "Nothing to remove")
claim_it "$nd"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$nd" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "nothing: ok"
assert_eq "$(jqf "$out" '.removed.worktree')" "false" "nothing: no worktree to remove"
assert_eq "$(jqf "$out" '.removed.branch')" "false" "nothing: no branch to remove"
assert_eq "$(state_of "$nd")" "todo" "nothing: the claim was still released"

# --- dry run: every refusal still runs, nothing is written -----------------
dr=$(new_story "$repo" "Dry run me")
wdr=$(mk_dispatched "$repo" "$dr")
claim_it "$dr"
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" --project "$slug" reset "$dr" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry: ok"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry: flagged"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "git worktree remove" "dry: previews the removal"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "git branch -D" "dry: previews the branch deletion"
wt_exists "$wdr" || fail_test "dry: worktree was actually removed"
assert_eq "$(state_of "$dr")" "in-progress" "dry: the story did NOT move"

echo scratch >"$repo/.claude/worktrees/$wdr/scratch.txt"
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" --project "$slug" reset "$dr" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dry-refusal: a preview refuses exactly where a real run would"
assert_eq "$(jqf "$out" .reason)" "dirty-worktree" "dry-refusal: for the same reason"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" reset 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "reset: missing id is ok:false"
assert_contains "$(jqf "$out" .display)" "usage:" "reset: missing id shows the usage line"
out=$(cd "$repo" && bash "$SCRIPT" reset "bad id!" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "reset: invalid id is ok:false"
assert_contains "$(jqf "$out" .display)" "alphanumeric" "reset: invalid id names the constraint"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" reset "$nd" junk 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "reset: a word that lands nowhere is refused"

finish
