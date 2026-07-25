#!/usr/bin/env bash
# Merged-on-origin, stale-locally: the case that forces the -d -> -D
# escalation in delete_merged_local_branch.
#
# A worktree-driven repo often never pulls: the PR merges on GitHub and local
# `main` lags indefinitely. branch_is_merged therefore checks the UNION of
# origin/<default> and local <default>, origin first, and complete freshens
# origin/<default> once before scanning. Plain `git branch -d` still refuses
# (the branch is merged into neither HEAD nor an upstream -- dispatch creates
# branches with --no-track), so the delete escalates to -D, but ONLY after
# re-confirming merged-ness.
#
# The fixture-validity assertions come FIRST and deliberately: if the fixture
# ever stopped reproducing the lag, every assertion below would pass
# vacuously against an ordinary already-merged branch and this regression
# would silently stop being tested.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
origin=$(cd "$repo" && git remote get-url origin)

id=$(new_story "$repo" "Merged on origin only")
w=$(mk_dispatched "$repo" "$id")
(cd "$repo/.claude/worktrees/$w" && echo x >x && git add x && git commit -qm "the work" \
  && git push -q origin "worktree-$w") >/dev/null 2>&1

# Land the merge on origin from a SEPARATE clone, standing in for GitHub —
# $repo itself never pulls, so its local main stays behind.
clone=$(mktemp -d /tmp/story-test-clone.XXXXXX)
_TMP_REPOS+=("$clone")
(
  git clone -q "$origin" "$clone/c"
  cd "$clone/c"
  git config user.email t@t
  git config user.name t
  git merge -q --no-ff -m "merge the work" "origin/worktree-$w"
  git push -q origin main
) >/dev/null 2>&1

# --- fixture validity: the lag must be real ---
(cd "$repo" && git merge-base --is-ancestor "refs/heads/worktree-$w" refs/heads/main 2>/dev/null) \
  && fail_test "fixture invalid: local main already contains the branch, so there is no lag to test"
local_main=$(cd "$repo" && git rev-parse refs/heads/main)
origin_main=$(cd "$repo" && git ls-remote origin main | cut -f1)
[ "$local_main" = "$origin_main" ] \
  && fail_test "fixture invalid: local main and origin main agree, so there is no lag to test"

# --- plan classifies it deletable, judged against origin, not local main ---
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$id" 2>&1)
assert_eq "$(jqf "$out" .plan.branch.status)" "deletable" \
  "stale base: branch merged on origin is deletable despite a lagging local main"

# Local main must still be lagging — complete freshens the remote-tracking
# ref only, and must never fast-forward or otherwise mutate a local branch.
assert_eq "$(cd "$repo" && git rev-parse refs/heads/main)" "$local_main" \
  "stale base: local main was not moved by the freshen"

# --- execute really deletes it, via the -D escalation ---
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "stale base: exec ok"
assert_eq "$(jqf "$out" '.removed.branches|length')" "1" "stale base: branch removed"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$w") \
  && fail_test "stale base: branch survived the escalation"

# --- GUARD: a genuinely unmerged branch is NOT swept up by the escalation ---
un=$(new_story "$repo" "Genuinely unmerged")
wun=$(mk_dispatched "$repo" "$un")
(cd "$repo/.claude/worktrees/$wun" && echo y >y && git add y && git commit -qm "never merged") >/dev/null 2>&1
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$un" 2>&1)
assert_eq "$(jqf "$out" '.removed.branches|length')" "0" "stale base: unmerged branch not deleted"
assert_contains "$(jqf "$out" '.skipped|join(" ")')" "unmerged" "stale base: and it is reported"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$wun") \
  || fail_test "stale base: an unmerged branch was deleted by the -D escalation"

finish
