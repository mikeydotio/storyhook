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

lease=$(jq -n --arg project "$slug" --arg story "$id" \
  --arg repository "$repository_path" --arg worktree "$worktree_path" \
  --arg branch "worktree-$name" --arg socket "$socket" \
  '{version:1,project_slug:$project,story_id:$story,
    repository_path:$repository,worktree_path:$worktree,branch:$branch,
    tmux:{socket_path:$socket}}')
printf '%s\n' "$lease" >"$private_git/storyhook-cleanup-lease-v1.json"
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

# The same durable lease is safe to replay after every owned resource is gone.
again=$(cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" \
  PATH="$TESTS_DIR/fakes:$PATH" bash "$SCRIPT" --project "$slug" reap "$id" 2>&1)
assert_eq "$(jqf "$again" .ok)" "true" "story-window reap is idempotent"
assert_eq "$(jqf "$again" '.removed.tmux')" "false" \
  "idempotent reap does not invent a tmux removal"
assert_eq "$(cat "$FAKE_TMUX_STATE/windows")" $'@12\tSH-OTHER' \
  "idempotent reap still preserves differently named windows"

# A vanished tmux server is also a proved-absent postcondition.
rm -f "$socket"
server_gone=$(cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" \
  PATH="$TESTS_DIR/fakes:$PATH" bash "$SCRIPT" --project "$slug" reap "$id" 2>&1)
assert_eq "$(jqf "$server_gone" .ok)" "true" \
  "story-window reap accepts an absent tmux server"

finish
