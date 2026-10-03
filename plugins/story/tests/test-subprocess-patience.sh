#!/usr/bin/env bash
# SH-863: the native resource probe honors this fixture's declared patience.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
slug=$(slug_for "$repo")
id=$(new_story "$repo" "Delayed resource identity")
wname=$(mk_dispatched "$repo" "$id")
printf 'uncommitted work\n' > "$repo/.claude/worktrees/$wname/scratch.txt"
(cd "$repo" && story claim "$id" --no-comment --json >/dev/null) || exit 1

# This is a deliberate stimulus beyond the production 3 s probe, not a wait
# for readiness. The patient attempt uses the shared harness declaration.
printf '4\n' > "$FAKE_TMUX_STATE/resource_delay"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$id" 2>&1)
assert_ok "$out" true "delayed probe: unclaim succeeds: $out"
assert_eq "$(jqf "$out" .worktree_status)" dirty "delayed probe: dirty worktree remains visible"
[ -f "$repo/.claude/worktrees/$wname/scratch.txt" ] || fail_test "delayed probe: work was removed"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$wname") || fail_test "delayed probe: branch was removed"
# Every probe was delayed, including the window-absence proof AFTER the
# release: the step SH-840 lost under gate load while this inventory still had
# a fixed 3 s bound. Named on its own so a regression there says which step.
case "$(jqf "$out" .display)" in
  *"could not prove"*) fail_test "delayed probe: the post-release window proof was not answered: $out" ;;
esac

# A floor below the production timeout must not shorten it or suppress a
# timeout. With no floor, the same unanswered identity also refuses safely.
for declaration in absent 1; do
  story daemon stop --force >/dev/null || exit 1
  if [ "$declaration" = absent ]; then
    unset STORYHOOK_TEST_SUBPROCESS_PATIENCE_MS
  else
    export STORYHOOK_TEST_SUBPROCESS_PATIENCE_MS="$declaration"
  fi
  (cd "$repo" && story claim "$id" --no-comment --json >/dev/null) || exit 1
  out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$id" 2>&1)
  assert_ok "$out" false "$declaration: a delayed identity refuses: $out"
  assert_eq "$(jqf "$out" .reason)" resource-identity-unsafe "$declaration: uncertainty is not absence"
  assert_contains "$(jqf "$out" .display)" "timed out" "$declaration: timeout diagnostic survives"
  state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
  assert_eq "$state" in-progress "$declaration: claim was not released"
  [ -f "$repo/.claude/worktrees/$wname/scratch.txt" ] || fail_test "$declaration: work was removed"
  (cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$wname") || fail_test "$declaration: branch was removed"
  # Release through the store verb only; the next case must begin unclaimed.
  (cd "$repo" && story unclaim "$id" --no-comment >/dev/null) || exit 1
done

finish
