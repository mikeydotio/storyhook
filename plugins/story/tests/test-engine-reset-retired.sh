#!/usr/bin/env bash
# SH-890: obsolete engine transport must not fall through to ordinary reset.
source "$(dirname "$0")/lib.sh"
repo=$(mk_story_repo)
slug=$(slug_for "$repo")
id=$(new_story "$repo" "Preserve obsolete reset target")
window=$(mk_dispatched "$repo" "$id")
(cd "$repo" && story claim "$id" --no-comment --json >/dev/null 2>&1)
printf 'keep stale transport work\n' >"$repo/.claude/worktrees/$window/keep.txt"
out=$(cd "$repo" && STORYHOOK_ENGINE_RESET_V1='{"token":"obsolete"}' bash "$SCRIPT" --project "$slug" reset "$id" --force 2>&1)
assert_ok "$out" "false" "obsolete engine reset input is refused"
assert_contains "$out" "engine-reset-retired" "refusal names the retired transport"
assert_eq "$(cat "$repo/.claude/worktrees/$window/keep.txt")" "keep stale transport work" "obsolete request preserves dirty work"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "in-progress" "obsolete request preserves claim"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$window") || fail_test "obsolete request removed the branch"
finish
