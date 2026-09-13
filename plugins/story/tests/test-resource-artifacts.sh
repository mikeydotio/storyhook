#!/usr/bin/env bash
# SH-709 + SH-708: protect actual resources across providers and absent paths.
source "$(dirname "$0")/lib.sh"
repo=$(mk_story_repo RAR)
manifest="$STORYHOOK_DATA_DIR/managed-paths"
for container in .claude/worktrees .codex/worktrees 'custom lane'; do
  id=$(new_story "$repo" "Protected $container")
  path="$repo/$container/$id"
  (cd "$repo" && git worktree add -q -b "worktree-$id" "$path" HEAD && story claim "$id" --no-comment >/dev/null) || exit 1
  mkdir -p "$path/protected"
  printf sentinel >"$path/protected/file"
  printf '%s\n' "$path/protected" >"$manifest"
  out=$(cd "$repo" && STORY_AGENT=unsupported bash "$SCRIPT" reset "$id" --force)
  assert_eq "$(jqf "$out" .reason)" installed-artifact-resource "$container: protect resolved removal target"
  assert_eq "$(cat "$path/protected/file")" sentinel "$container: artifact survives"
  assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" in-progress "$container: claim survives"
done
# No worktree means no recursive removal; Git writes still require validation.
printf '%s\n' "$HOME/unrelated-installed" >"$manifest"
id=$(new_story "$repo" 'Branch-only cleanup on installed host')
(cd "$repo" && git branch "worktree-$id" && story claim "$id" --no-comment >/dev/null) || exit 1
out=$(cd "$repo" && STORY_AGENT=unsupported bash "$SCRIPT" reset "$id")
assert_eq "$(jqf "$out" .ok)" true 'branch-only reset succeeds'
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$id") && fail_test 'branch-only branch survives'
id=$(new_story "$repo" 'Absent resource on installed host')
(cd "$repo" && story claim "$id" --no-comment >/dev/null) || exit 1
out=$(cd "$repo" && STORY_AGENT=unsupported bash "$SCRIPT" reset "$id")
assert_eq "$(jqf "$out" .ok)" true 'absent reset succeeds'
finish
