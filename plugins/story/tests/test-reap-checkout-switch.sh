#!/usr/bin/env bash
# A reap result may say ok:true only when every discovered cleanup target was
# reclaimed. Re-linking a project to a second clone must not turn the original
# story worktree and branch into invisible litter, nor may a concrete removal
# error be wrapped in success for the centralized verifier to misclassify.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo RCS)
slug=$(slug_for "$repo")

# Nearest non-failing comparison: with the registered checkout unchanged, reap
# removes both same-repository resources and truthfully reports success.
same_id=$(new_story "$repo" "Same-checkout reap")
same_name=$(mk_dispatched "$repo" "$same_id")
(cd "$repo" && story move "$same_id" done >/dev/null)
out=$(cd "$repo/.claude/worktrees/$same_name" \
  && bash "$SCRIPT" --project "$slug" reap "$same_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "same checkout: reap succeeds"
assert_eq "$(jqf "$out" '.removed.worktree')" "true" \
  "same checkout: worktree is removed"
assert_eq "$(jqf "$out" '.removed.branch')" "true" \
  "same checkout: branch is removed"
[ ! -d "$repo/.claude/worktrees/$same_name" ] \
  || fail_test "same checkout: worktree survived"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$same_name") \
  && fail_test "same checkout: branch survived"

# Regression: the verifier can temporarily register a different clone before
# it asks the original agent worktree to reap itself. A same-named branch that
# is checked out at an unexpected path makes the new clone's failed deletion
# explicit and deterministic; the original clone's real resources must either
# be found and removed, or force an honest ok:false result.
switched_id=$(new_story "$repo" "Checkout switched before reap")
switched_name=$(mk_dispatched "$repo" "$switched_id")
(cd "$repo" && story move "$switched_id" done >/dev/null)

other_root=$(mktemp -d /tmp/story-test.checkout-switch.XXXXXX)
_register_tmp "$other_root"
other="$other_root/checkout"
git clone -q "$(cd "$repo" && git config --get remote.origin.url)" "$other"
git -C "$other" config user.email t@t
git -C "$other" config user.name t
git -C "$other" worktree add -q -b "worktree-$switched_name" \
  "$other_root/unexpected-worktree" main
story --project "$slug" project link checkout "$other" >/dev/null

out=$(cd "$repo/.claude/worktrees/$switched_name" \
  && bash "$SCRIPT" --project "$slug" reap "$switched_id" 2>&1)

original_survived=false
[ -d "$repo/.claude/worktrees/$switched_name" ] && original_survived=true
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$switched_name") \
  && original_survived=true

if [ "$original_survived" = true ]; then
  assert_eq "$(jqf "$out" .ok)" "false" \
    "switched checkout: surviving original resources forbid cleanup-complete"
else
  assert_eq "$(jqf "$out" .ok)" "true" \
    "switched checkout: success requires removal of the original resources"
fi

branch_error=$(jqf "$out" '.branch_error // ""')
if [ -n "$branch_error" ]; then
  assert_eq "$(jqf "$out" .ok)" "false" \
    "switched checkout: branch_error forbids ok:true"
fi

finish
