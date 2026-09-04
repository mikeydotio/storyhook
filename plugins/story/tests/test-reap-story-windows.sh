#!/usr/bin/env bash
# A done story owns every tmux window carrying its exact story id. Reaping is
# exhaustive by name: duplicate windows are all garbage, while differently
# named windows remain untouched. Git cleanup stays bound to the exact lease.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo RSW)
slug=$(slug_for "$repo")
id=$(new_story "$repo" "Reap every story window")
name=$(mk_dispatched "$repo" "$id")
worktree="$repo/.claude/worktrees/$name"
private_git=$(git -C "$worktree" rev-parse --absolute-git-dir)
repository_path=$(cd "$repo" && pwd -P)
worktree_path=$(cd "$worktree" && pwd -P)
socket="$FAKE_TMUX_STATE/tmux.sock"
touch "$socket"
printf '@10\t%s\n@11\t%s\n@12\tSH-OTHER\n' "$id" "$id" >"$FAKE_TMUX_STATE/windows"

jq -n --arg project "$slug" --arg story "$id" \
  --arg repository "$repository_path" --arg worktree "$worktree_path" \
  --arg branch "worktree-$name" --arg socket "$socket" \
  '{version:1,project_slug:$project,story_id:$story,
    repository_path:$repository,worktree_path:$worktree,branch:$branch,
    tmux:{socket_path:$socket}}' \
  >"$private_git/storyhook-cleanup-lease-v1.json"
(cd "$repo" && story move "$id" done >/dev/null)

out=$(cd "$worktree" && PATH="$TESTS_DIR/fakes:$PATH" \
  bash "$SCRIPT" --project "$slug" reap "$id" 2>&1)
status=$?

assert_eq "$status" "0" "story-window reap exits successfully"
assert_eq "$(jqf "$out" .ok)" "true" "story-window reap reports success"
assert_eq "$(jqf "$out" '.removed.tmux')" "true" \
  "story-window reap reports that it removed tmux windows"
assert_eq "$(jqf "$out" '.postconditions.tmux_story_windows_absent')" "true" \
  "story-window reap proves no same-story window remains"
assert_eq "$(cat "$FAKE_TMUX_STATE/windows")" $'@12\tSH-OTHER' \
  "story-window reap preserves differently named windows"
assert_contains "$(cat "$FAKE_TMUX_STATE/kill_window_args.log")" "-t @10" \
  "story-window reap removes the first matching window"
assert_contains "$(cat "$FAKE_TMUX_STATE/kill_window_args.log")" "-t @11" \
  "story-window reap removes the second matching window"
case "$(cat "$FAKE_TMUX_STATE/kill_window_args.log")" in
  *'@12'*) fail_test "story-window reap removed an unrelated window" ;;
esac
[ ! -e "$worktree" ] || fail_test "story-window reap left its exact worktree"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$name") \
  && fail_test "story-window reap left its exact branch"

finish
