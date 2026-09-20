#!/usr/bin/env bash
# SH-743: a damaged registration holds its owner, not the whole dispatch batch.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
damaged=$(new_story "$repo" "Damaged registration")
independent=$(new_story "$repo" "Independent dispatch")
outside=$(mktemp -d /tmp/story-test.XXXXXX)
_register_tmp "$outside"
outside=$(cd "$outside" && pwd -P)
path="$outside/custom-worktree"
branch="worktree-$damaged"
(cd "$repo" && git worktree add -q -b "$branch" "$path" HEAD) || exit 1
rm "$path/.git"

dispatch_case() {
  local id="$1"
  (
    cd "$repo" || exit 1
    TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id" --agent=claude 2>&1
  )
}

before=$(cd "$repo" && story show "$damaged" --json | jq -r '.story.story.state')
refused=$(dispatch_case "$damaged")
assert_eq "$(jqf "$refused" .reason)" resource-identity-unsafe \
  "affected story refuses damaged registration"
assert_contains "$(jqf "$refused" .display)" "stale registration" \
  "refusal names the failed invariant"
assert_eq "$(cd "$repo" && story show "$damaged" --json | jq -r '.story.story.state')" \
  "$before" "refusal preserves tracker state"
(cd "$repo" && story claim "$damaged" --no-comment >/dev/null) || exit 1
reset=$(cd "$repo" && bash "$SCRIPT" reset "$damaged" --force)
assert_eq "$(jqf "$reset" .reason)" resource-identity-unsafe \
  "cleanup refuses the damaged target even with force"
assert_eq "$(cd "$repo" && story show "$damaged" --json | jq -r '.story.story.state')" \
  in-progress "refused cleanup preserves the claim"

started=$(dispatch_case "$independent")
assert_eq "$(jqf "$started" .ok)" true "independent story dispatches"
assert_eq "$(cd "$repo" && story show "$independent" --json | jq -r '.story.story.state')" \
  in-progress "independent claim advances"
[ -d "$path" ] || fail_test "damaged worktree directory was removed"
[ ! -e "$path/.git" ] || fail_test "damaged .git link was rebuilt"
(cd "$repo" && git worktree list --porcelain | rg -F -q "worktree $path") \
  || fail_test "damaged Git registration was pruned"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/$branch") \
  || fail_test "damaged branch was deleted"

finish
