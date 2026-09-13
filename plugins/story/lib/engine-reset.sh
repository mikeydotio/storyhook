# Engine-only reset actuator. The daemon retains the claim until this helper
# proves cleanup. Ordinary reset's conflict-means-delete policy is unsuitable
# here: a verifying conflict means the verifier owns every resource.
engine_reset_authorized() {
  local request="$1" run token observed
  run=$(printf '%s' "$request" | jq -er '.run_id') || return 1
  token=$(printf '%s' "$request" | jq -er '.token') || return 1
  observed=$(story_cli engine reset-target --run "$run" --token "$token" --json) || {
    printf 'engine reset authorization failed: %s\n' "$observed" >&2
    return 1
  }
  printf '%s' "$observed" | jq -e --argjson requested "$request" '
    .result == "ok" and ((.reset | del(.failure)) == ($requested | del(.failure)))' >/dev/null || {
      printf 'engine reset authorization mismatch for run %s token %s\n' "$run" "$token" >&2
      return 1
    }
}

cmd_engine_reset() {
  local id="$1" request="$2" lease caller_dir caller_socket caller_window auth_error
  caller_dir=$(pwd -P) || fail "cannot determine reset caller directory"
  [ -z "$DRY_RUN" ] || refuse "engine-reset-dry-run" "engine reset requires a live reservation, not a dry run"
  lease=$(printf '%s' "$request" | jq -ce '.lease') || refuse "invalid-reset" "engine reset request has no lease"
  validate_cleanup_lease reset "$id" "$lease"
  auth_error=$(engine_reset_authorized "$request" 2>&1) || refuse "reset-ownership" "engine reset reservation no longer matches this story, run and lease: $auth_error"

  local worktree="$LEASED_WORKTREE" branch="$LEASED_BRANCH" repository="$LEASED_REPO"
  [ "$caller_dir" != "$worktree" ] || refuse "current-worktree" "engine reset cannot remove its calling worktree"
  case "$caller_dir/" in "$worktree/"*) refuse "current-worktree" "engine reset caller is inside its target worktree" ;; esac
  local common artifact_error default marker private_git
  common=$(git rev-parse --path-format=absolute --git-common-dir 2>&1) || refuse "reset-git-metadata" "$common"
  artifact_error=$(python3 "$STORY_PLUGIN_ROOT/lib/artifact-resources.py" "$repository" "$common" "$worktree" 2>&1) \
    || refuse "installed-artifact-resource" "engine reset refused: $artifact_error"
  default=$(default_branch 2>&1) || refuse "default-branch-unknown" "engine reset cannot identify protected branches: $default"
  is_protected_branch "$branch" "$default" && refuse "protected-branch" "engine reset cannot delete protected branch $branch"

  if registered_worktree_branch "$worktree" >/dev/null 2>&1; then
    private_git=$(git -C "$worktree" rev-parse --absolute-git-dir 2>&1) || refuse "reset-private-git" "$private_git"
    marker="$private_git/$CLEANUP_LEASE_MARKER"
    [ -f "$marker" ] && [ ! -L "$marker" ] || refuse "reset-lease-marker" "engine reset requires the original regular cleanup marker at $marker"
    jq -e --argjson expected "$lease" '. == $expected' "$marker" >/dev/null \
      || refuse "reset-lease-marker" "engine reset marker changed at $marker"
  fi

  leased_story_windows "$lease" "$id" || refuse "reset-tmux-unverifiable" "engine reset cannot enumerate the leased tmux server"
  local windows="$LEASE_TMUX_WINDOWS" socket window
  case "$windows" in *$'\n'*) refuse "reset-window-ambiguous" "engine reset found multiple exact-name windows on its leased server" ;; esac
  socket=$(printf '%s' "$lease" | jq -r '.tmux.socket_path')
  if [ -n "${TMUX_PANE:-}" ]; then
    caller_socket=$(tmux display-message -p -t "$TMUX_PANE" '#{socket_path}' 2>/dev/null) \
      || refuse "reset-self-unverifiable" "engine reset cannot identify its calling tmux server"
    caller_window=$(tmux display-message -p -t "$TMUX_PANE" '#{window_id}' 2>/dev/null) \
      || refuse "reset-self-unverifiable" "engine reset cannot identify its calling tmux window"
    if python3 -c 'import os,sys; sys.exit(os.path.realpath(sys.argv[1]) != os.path.realpath(sys.argv[2]))' "$socket" "$caller_socket"; then
      while IFS= read -r window; do
        [ "$window" != "$caller_window" ] || refuse "self-window" "engine reset cannot close its calling tmux window"
      done <<< "$windows"
    fi
  fi
  auth_error=$(engine_reset_authorized "$request" 2>&1) || refuse "reset-ownership" "engine reset lost ownership before closing its window: $auth_error"
  if [ -n "$windows" ]; then
    while IFS= read -r window; do
      # Never reacquire a replacement by name after preflight.
      leased_story_windows "$lease" "$id" || refuse "reset-tmux-unverifiable" "engine reset cannot recheck window ownership"
      [ "$LEASE_TMUX_WINDOWS" = "$windows" ] || refuse "reset-window-replaced" "engine reset window inventory changed before closure"
      tmux -S "$socket" kill-window -t "$window" >/dev/null 2>&1 \
        || refuse "reset-window-kill" "engine reset could not close window $window on $socket"
    done <<< "$windows"
  fi
  leased_story_windows "$lease" "$id" || refuse "reset-tmux-unverifiable" "engine reset cannot prove window absence"
  [ -z "$LEASE_TMUX_WINDOWS" ] || refuse "reset-window-remains" "engine reset will not remove Git resources while agent windows remain"

  # Revalidate Git identity after the agent has stopped changing its files.
  validate_cleanup_lease reset "$id" "$lease"
  auth_error=$(engine_reset_authorized "$request" 2>&1) || refuse "reset-ownership" "engine reset lost ownership before Git cleanup: $auth_error"
  if registered_worktree_branch "$worktree" >/dev/null 2>&1; then
    [ -f "$marker" ] && [ ! -L "$marker" ] \
      || refuse "reset-lease-marker" "engine reset cleanup marker disappeared after window closure"
    [ "$(git -C "$worktree" rev-parse --absolute-git-dir)" = "$private_git" ] \
      || refuse "reset-private-git" "engine reset worktree registration changed after window closure"
    jq -e --argjson expected "$lease" '. == $expected' "$marker" >/dev/null \
      || refuse "reset-lease-marker" "engine reset marker changed after window closure"
    local failure
    failure=$(git worktree remove --force --force "$worktree" 2>&1) \
      || refuse "reset-worktree-remains" "engine reset could not remove $worktree: $failure"
  fi
  if local_branch_exists "$branch"; then
    local failure
    failure=$(git branch -D -- "$branch" 2>&1) \
      || refuse "reset-branch-remains" "engine reset could not delete $branch: $failure"
  fi
  registered_worktree_branch "$worktree" >/dev/null 2>&1 \
    && refuse "reset-registration-remains" "engine reset left a registration for $worktree"
  [ ! -e "$worktree" ] && [ ! -L "$worktree" ] || refuse "reset-path-remains" "engine reset left a path at $worktree"
  local_branch_exists "$branch" && refuse "reset-branch-remains" "engine reset left branch $branch"
  leased_story_windows "$lease" "$id" || refuse "reset-tmux-unverifiable" "engine reset cannot prove final window absence"
  [ -z "$LEASE_TMUX_WINDOWS" ] || refuse "reset-window-remains" "engine reset found an agent window after Git cleanup"
  jq -n --argjson request "$request" '{ok:true, token:$request.token, lease:$request.lease,
    postconditions:{tmux_story_windows_absent:true,worktree_registration_absent:true,worktree_path_absent:true,branch_absent:true},
    display:"Exact engine reset resources are absent; the daemon may now restore the story."}'
}
