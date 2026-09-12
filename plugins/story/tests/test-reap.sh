#!/usr/bin/env bash
# `story.sh reap <id>` (SH-208) — the autonomous charter's own last act:
# reclaim a CLOSED story's worktree, branch and tmux window. Unlike
# `complete execute`, refusal is ALL-OR-NOTHING: every guard case below
# must leave the worktree AND the branch exactly as it found them, never a
# partial cleanup nobody is left to observe. There is no comment assertion
# anywhere in this file on purpose -- reap has no record step to assert. It
# once had no choice, because a closed story refused a comment; since SH-261
# it would be accepted, and the absence is a design decision instead (see
# `reap`'s own block in bin/story.sh). If a record step is ever added, this
# file is where its assertion belongs.
source "$(dirname "$0")/lib.sh"

export PATH="$TESTS_DIR/fakes:$PATH"
export FAKE_TMUX_STATE
FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")

repo=$(mk_story_repo)

# close_story <id> — move a story straight to the project's CLOSED state,
# bypassing dispatch/complete entirely; reap's own preflight is what this
# file is testing, not the state machine that gets a story there.
close_story() { (cd "$repo" && story move "$1" done --json >/dev/null 2>&1); }

# --- GUARD: refuses an open story, touching nothing ------------------------
op=$(new_story "$repo" "Still open")
wop=$(mk_dispatched "$repo" "$op")
out=$(cd "$repo" && bash "$SCRIPT" reap "$op" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "open: ok:false"
assert_eq "$(jqf "$out" .reason)" "not-closed" "open: reason is not-closed"
[ -d "$repo/.claude/worktrees/$wop" ] || fail_test "open: worktree was removed anyway"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$wop") \
  || fail_test "open: branch was removed anyway"

# --- GUARD: an abandonment state is CLOSED, but it is not completion ------
#
# SH-508: two autonomous sessions ignored the charter's literal `done`
# command, moved worked stories to the `closed` abandonment state, and then
# successfully reaped the only workspace that could still have corrected the
# semantic mistake. Superstate alone cannot decide that cleanup is warranted:
# reap must agree with the same project-specific completion-state resolver the
# charter and `complete execute` use.
ab=$(new_story "$repo" "Abandoned, not completed")
wab=$(mk_dispatched "$repo" "$ab")
(cd "$repo" && story close "$ab" "the work was abandoned" >/dev/null 2>&1)
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" reap "$ab" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "abandonment: ok:false"
assert_eq "$(jqf "$out" .reason)" "not-completion-state" \
  "abandonment: reason distinguishes closure from completion"
assert_eq "$(jqf "$out" .state)" "dropped" "abandonment: reports the state found"
assert_eq "$(jqf "$out" .completion_state)" "done" \
  "abandonment: reports the state required"
[ -d "$repo/.claude/worktrees/$wab" ] \
  || fail_test "abandonment: worktree was removed anyway"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$wab") \
  || fail_test "abandonment: branch was removed anyway"

# --- GUARD: a dirty worktree refuses, uncommitted work survives ------------
dy=$(new_story "$repo" "Dirty")
wdy=$(mk_dispatched "$repo" "$dy")
close_story "$dy"
echo scratch >"$repo/.claude/worktrees/$wdy/scratch.txt"
out=$(cd "$repo" && bash "$SCRIPT" reap "$dy" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dirty: ok:false"
assert_eq "$(jqf "$out" .reason)" "dirty-worktree" "dirty: reason is dirty-worktree"
[ -d "$repo/.claude/worktrees/$wdy" ] || fail_test "dirty: worktree was removed anyway"
[ -f "$repo/.claude/worktrees/$wdy/scratch.txt" ] || fail_test "dirty: uncommitted file was destroyed"

# --- GUARD: an unmerged branch refuses -- worktree ALSO untouched, since --
# reap is all-or-nothing, unlike complete's partial best-effort cleanup.
un=$(new_story "$repo" "Unmerged")
wun=$(mk_dispatched "$repo" "$un")
(cd "$repo/.claude/worktrees/$wun" && echo x >x && git add x && git commit -qm work) >/dev/null 2>&1
close_story "$un"
unsha=$(cd "$repo" && git rev-parse "refs/heads/worktree-$wun")
out=$(cd "$repo" && bash "$SCRIPT" reap "$un" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unmerged: ok:false"
assert_eq "$(jqf "$out" .reason)" "unmerged-branch" "unmerged: reason is unmerged-branch"
[ -d "$repo/.claude/worktrees/$wun" ] || fail_test "unmerged: worktree was removed despite the branch refusal"
assert_eq "$(cd "$repo" && git rev-parse "refs/heads/worktree-$wun")" "$unsha" \
  "unmerged: branch survives at the same commit — the work is not lost"

# --- GUARD: a locked worktree refuses, and is never unlocked ---------------
lk=$(new_story "$repo" "Locked")
wlk=$(mk_dispatched "$repo" "$lk")
close_story "$lk"
(cd "$repo" && git worktree lock ".claude/worktrees/$wlk") >/dev/null 2>&1
out=$(cd "$repo" && bash "$SCRIPT" reap "$lk" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "locked: ok:false"
assert_eq "$(jqf "$out" .reason)" "locked-worktree" "locked: reason is locked-worktree"
[ -d "$repo/.claude/worktrees/$wlk" ] || fail_test "locked: worktree was removed"
assert_contains "$(cd "$repo" && git worktree list --porcelain)" "locked" "locked: still locked afterwards"

# --- dry run previews without touching anything -----------------------------
dr=$(new_story "$repo" "Dry run me")
wdr=$(mk_dispatched "$repo" "$dr")
close_story "$dr"
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" reap "$dr" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry: ok"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry: flagged"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "git worktree remove" "dry: previews the removal"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "kill-window" "dry: previews the window close"
[ -d "$repo/.claude/worktrees/$wdr" ] || fail_test "dry: worktree was actually removed"

# --- happy path: `current` is NOT a veto, and every step actually runs -----
# reap is invoked FROM INSIDE the worktree about to be reclaimed -- the exact
# shape the autonomous charter itself runs under -- so _story_worktree_status
# classifies it "current". _complete_prepare's own enter_checkout has already
# moved THIS process to the main checkout before the removal runs, which is
# what makes that safe (see cmd_reap's header comment).
hp=$(new_story "$repo" "Happy path")
whp=$(mk_dispatched "$repo" "$hp")
close_story "$hp"
out=$(cd "$repo/.claude/worktrees/$whp" \
  && TMUX=fake TMUX_PANE=%0 \
     FAKE_TMUX_PANES="$(printf '%s\t1\t%%7' "$whp")" \
     bash "$SCRIPT" --project "$(slug_for "$repo")" reap "$hp" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "happy: ok"
assert_eq "$(jqf "$out" '.removed.worktree')" "true" "happy: worktree removed"
assert_eq "$(jqf "$out" '.removed.branch')" "true" "happy: branch removed"
assert_contains "$(jqf "$out" .display)" "removed worktree" "happy: display names what it did"
[ -d "$repo/.claude/worktrees/$whp" ] && fail_test "happy: worktree still on disk"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$whp") \
  && fail_test "happy: branch still in git"
grep -q -- '-t @1' "$FAKE_TMUX_STATE/kill_window_args.log" \
  || fail_test "happy: tmux kill-window did not target the resolved window"

# --- idempotent: nothing to reclaim is still ok:true, not an error ---------
nt=$(new_story "$repo" "Nothing to reclaim")
close_story "$nt"
out=$(cd "$repo" && bash "$SCRIPT" reap "$nt" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "nothing-to-reclaim: ok"
assert_eq "$(jqf "$out" '.removed.worktree')" "false" "nothing-to-reclaim: no worktree to remove"
assert_eq "$(jqf "$out" '.removed.branch')" "false" "nothing-to-reclaim: no branch to remove"

# --- completion is the required `done`, never the first CLOSED state (SH-652)
# `shipped` is ordered ahead of `done`; a story a person moved into it is not
# completed work and reap refuses it, while `done` is still accepted.
custom=$(new_story "$repo" "Custom closed state is not completion")
wcustom=$(mk_dispatched "$repo" "$custom")
(cd "$repo" \
  && story state add shipped --super CLOSED >/dev/null \
  && story state reorder todo,in-progress,verifying,blocked,shipped,done,dropped >/dev/null \
  && story move "$custom" shipped >/dev/null)
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" reap "$custom" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "shipped-first catalog: a story in shipped is refused"
assert_eq "$(jqf "$out" .reason)" "not-completion-state" "shipped-first catalog: names the reason"
assert_eq "$(jqf "$out" .completion_state)" "done" "shipped-first catalog: names done as completion"
[ -d "$repo/.claude/worktrees/$wcustom" ] \
  || fail_test "shipped-first catalog: a refused reap removed the worktree"
stilldone=$(new_story "$repo" "Done under a shipped-first catalog")
wdone=$(mk_dispatched "$repo" "$stilldone")
close_story "$stilldone"
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" reap "$stilldone" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "shipped-first catalog: done is still completion"
assert_eq "$(jqf "$out" .dry_run)" "true" "shipped-first catalog: reaches the dry-run cleanup"
[ -d "$repo/.claude/worktrees/$wdone" ] \
  || fail_test "shipped-first catalog: dry run removed the worktree"

# --- SH-691: merged-ness is judged against origin's ADVERTISED default -------
# The local origin/HEAD cache says `main`; origin's default is `dev`, and the
# branch is merged only there. A reap that trusted the cache — or the old
# `main` literal — would refuse this as unmerged.
origin=$(git -C "$repo" remote get-url origin)
dv=$(new_story "$repo" "Merged into dev only")
wdv=$(mk_dispatched "$repo" "$dv")
printf 'dev\n' >"$repo/.claude/worktrees/$wdv/dev.txt"
git -C "$repo/.claude/worktrees/$wdv" add dev.txt
git -C "$repo/.claude/worktrees/$wdv" -c user.name=t -c user.email=t@e commit -qm 'dev work'
git -C "$repo" push -q origin "refs/heads/worktree-$wdv:refs/heads/dev"
git --git-dir="$origin" symbolic-ref HEAD refs/heads/dev
assert_eq "$(git -C "$repo" symbolic-ref refs/remotes/origin/HEAD)" "refs/remotes/origin/main" \
  "dev-only: fixture cache still says main"
close_story "$dv"
out=$(cd "$repo" && bash "$SCRIPT" reap "$dv" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dev-only: reaped — merged-ness was judged against origin's default: $out"
assert_eq "$(jqf "$out" '.removed.branch')" "true" "dev-only: the branch merged only into dev was deleted"
[ -d "$repo/.claude/worktrees/$wdv" ] && fail_test "dev-only: worktree survived a successful reap"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$wdv") \
  && fail_test "dev-only: branch survived a successful reap"
git --git-dir="$origin" symbolic-ref HEAD refs/heads/main

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" reap 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "reap: missing id is ok:false"
out=$(cd "$repo" && bash "$SCRIPT" reap "bad id!" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "reap: invalid id is ok:false"
assert_contains "$(jqf "$out" .display)" "alphanumeric" "reap: invalid id names the constraint"

finish
