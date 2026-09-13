#!/usr/bin/env bash
# SH-709: real socket/pane identity through the production helper.
resource_test_tmux=$(command -v tmux)
source "$(dirname "$0")/lib.sh"
mkdir -p "$HOME/bin" "$HOME/private-tmux"
ln -s "$resource_test_tmux" "$HOME/bin/tmux"
export PATH="$HOME/bin:$PATH" TMUX_TMPDIR="$HOME/private-tmux"
repo=$(mk_story_repo)
slug=$(slug_for "$repo")
# The same server-local pane ID/name can exist on the caller and owner servers.
# The marker binds capture/reset to the owner, even from an unrelated terminal.
id=$(new_story "$repo" "Recorded server")
path="$repo/custom lane/$id"
(cd "$repo" && git worktree add -q -b "worktree-$id" "$path" HEAD && story claim "$id" --no-comment >/dev/null)
owner="$HOME/owner.sock"
caller="$HOME/caller.sock"
owner_pane=$(tmux -f /dev/null -S "$owner" new-session -d -s owned -n "$id" -c "$path" -P -F '#{pane_id}' 'printf "owned transcript\n"; sleep 120')
caller_pane=$(tmux -f /dev/null -S "$caller" new-session -d -s caller -n "$id" -c "$repo" -P -F '#{pane_id}' 'sleep 120')
# Register exact owned servers with this fixture's existing cleanup trap.
_resource_cleanup() {
  tmux -S "$owner" kill-server >/dev/null 2>&1 || :
  tmux -S "$caller" kill-server >/dev/null 2>&1 || :
  _cleanup
}
trap _resource_cleanup EXIT
tmux -S "$owner" set-window-option -t "$owner_pane" @storyhook-agent codex
repo_real=$(cd "$repo" && pwd -P)
path_real=$(cd "$path" && pwd -P)
gitdir=$(cd "$path" && git rev-parse --absolute-git-dir)
jq -nc --arg project "$slug" --arg id "$id" --arg repo "$repo_real" --arg path "$path_real" --arg socket "$owner" \
  '{version:1,project_slug:$project,story_id:$id,repository_path:$repo,worktree_path:$path,branch:("worktree-"+$id),tmux:{socket_path:$socket}}' > "$gitdir/storyhook-cleanup-lease-v1.json"
out=$(cd "$repo" && TMUX="$caller,0,0" TMUX_PANE="$caller_pane" STORY_AGENT=unsupported bash "$SCRIPT" capture "$id")
assert_eq "$(jqf "$out" .ok)" true "recorded socket: capture succeeds"
assert_contains "$(jqf "$out" .transcript)" "owned transcript" "recorded socket: captures owner"
out=$(cd "$repo" && TMUX="$caller,0,0" TMUX_PANE="$caller_pane" STORY_AGENT=claude bash "$SCRIPT" reset "$id")
if [ "$(jqf "$out" .ok)" != true ]; then printf '%s\n' "$out"; fi
assert_eq "$(jqf "$out" .ok)" true "recorded socket: reset succeeds despite identical caller pane ID"
[ ! -e "$path" ] || fail_test "recorded socket: worktree survived"
tmux -S "$caller" has-session -t caller || fail_test "recorded socket: unrelated caller server was touched"
finish
