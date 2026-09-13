# Existing-resource observations come from the native reader. Provider launch
# configuration must never choose what a lifecycle command inspects or removes.
RESOURCE_SOCKET=""
RESOURCE_CALLER_TMUX="${TMUX:-}"
RESOURCE_CALLER_SOCKET="$RESOURCE_CALLER_TMUX"
RESOURCE_CALLER_SOCKET="${RESOURCE_CALLER_SOCKET%%,*}"
RESOURCE_CALLER_PANE="${TMUX_PANE:-}"

# All terminal operations after discovery use the selected server, including
# provider protocol helpers that otherwise inherit tmux's ambient target.
tmux() {
  if [ -n "$RESOURCE_SOCKET" ] && [ "${1:-}" != -S ]; then
    command tmux -S "$RESOURCE_SOCKET" "$@"
  else
    command tmux "$@"
  fi
}

load_story_resources() {
  local id="$1" lease="${2:-}" result state legacy_socket
  if legacy_socket=$(command tmux display-message -p '#{socket_path}' 2>/dev/null); then
    case "$legacy_socket" in /*) ;; *) legacy_socket="" ;; esac
  else
    legacy_socket=""
  fi
  if [ -n "$RESOURCE_CALLER_PANE" ]; then
    case "$RESOURCE_CALLER_SOCKET" in /*) ;; *) RESOURCE_CALLER_SOCKET="$legacy_socket" ;; esac
  fi
  local -a args=(resources "$id" --json)
  [ -z "$legacy_socket" ] || args+=(--tmux-socket "$legacy_socket")
  [ -z "$lease" ] || args+=(--lease-json "$lease")
  [ -z "${WINDOW_NAME_TPL:-}" ] || args+=(--window-name "$(resolve_wname "$id")")
  [ -z "${STORY_WORKTREE_IGNORE_PATH:-}" ] || args+=(--worktree-root "$STORY_WORKTREE_IGNORE_PATH")
  if ! result=$(story_cli "${args[@]}"); then
    refuse "resource-query-failed" "cannot inspect existing resources for $id: $result"
  fi
  state=$(printf '%s' "$result" | jq -r '.resources.status // empty')
  case "$state" in
    resolved|absent) ;;
    *) refuse_with "resource-identity-unsafe" "existing resource identity for $id is ${state:-unavailable}: $(printf '%s' "$result" | jq -r '.resources.diagnostics | join("; ")'); no mutation is safe" "$(printf '%s' "$result" | jq '{resources:.resources}')" ;;
  esac
  RESOURCE_ID=$(printf '%s' "$result" | jq -r '.resources.story_id')
  # Published to story.sh callers of this sourced library.
  # shellcheck disable=SC2034
  RESOURCE_REPOSITORY=$(printf '%s' "$result" | jq -r '.resources.repository // empty')
  RESOURCE_WORKTREE=$(printf '%s' "$result" | jq -r '.resources.worktree // empty')
  # Published to story.sh callers of this sourced library.
  # shellcheck disable=SC2034
  RESOURCE_BRANCH=$(printf '%s' "$result" | jq -r '.resources.branch // empty')
  RESOURCE_WINDOW=$(printf '%s' "$result" | jq -r '.resources.window_name')
  # Published to story.sh callers of this sourced library.
  # shellcheck disable=SC2034
  RESOURCE_PROVIDER=$(printf '%s' "$result" | jq -r '.resources.provider // empty')
  RESOURCE_SOCKET=$(printf '%s' "$result" | jq -r '.resources.socket_path // empty')
  RESOURCE_PANE=$(printf '%s' "$result" | jq -r '.resources.pane.pane_id // empty')
  RESOURCE_REPORT=$(printf '%s' "$result" | jq -c '.resources')
  RESOURCE_LEASE="$lease"
  if [ -n "$RESOURCE_SOCKET" ]; then
    export TMUX="$RESOURCE_SOCKET,0,0"
  elif [ -n "$RESOURCE_CALLER_TMUX" ]; then
    export TMUX="$RESOURCE_CALLER_TMUX"
  else
    unset TMUX
  fi
}

# The native observation owns both confirmed absence and the exact target.
resource_find_pane() {
  printf '%s' "$RESOURCE_PANE"
}

resource_is_self() {
  [ -n "$RESOURCE_CALLER_PANE" ] || return 1
  local caller_socket
  caller_socket=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$RESOURCE_CALLER_SOCKET") || return 1
  [ -z "$RESOURCE_SOCKET" ] || [ "$RESOURCE_SOCKET" = "$caller_socket" ] || return 1
  [ "$1" != "$RESOURCE_CALLER_PANE" ] || return 0
  local caller_window target_window
  caller_window=$(tmux display-message -p -t "$RESOURCE_CALLER_PANE" '#{window_id}') || return 1
  target_window=$(tmux display-message -p -t "$1" '#{window_id}') || return 1
  [ -n "$caller_window" ] && [ "$caller_window" = "$target_window" ]
}

# Re-observe identity before a mutation. State/dirty/merge policy remains the
# caller's responsibility; this rejects a moved worktree or replaced pane.
revalidate_story_resources() {
  local identity before after pane_before pane_after
  identity='{repository,worktree,branch,socket_path,pane}'
  before=$(printf '%s' "$RESOURCE_REPORT" | jq -c "$identity")
  pane_before=$(resource_find_pane) || refuse "resource-query-failed" "cannot recheck story window before mutation"
  load_story_resources "$RESOURCE_ID" "$RESOURCE_LEASE"
  after=$(printf '%s' "$RESOURCE_REPORT" | jq -c "$identity")
  pane_after=$(resource_find_pane) || refuse "resource-query-failed" "cannot recheck story window before mutation"
  [ "$before" = "$after" ] && [ "$pane_before" = "$pane_after" ] \
    || refuse "resource-identity-changed" "story resources changed during preflight; retry from a fresh inventory"
}

# Keep a previously verified Git identity when teardown has removed its branch.
# This observation lease is local to the command and is never persisted.
resource_window_snapshot() {
  local lease="$RESOURCE_LEASE" result
  [ -n "$lease" ] || lease=$(printf '%s' "$RESOURCE_REPORT" | jq -c '.candidates[0].lease // empty')
  if [ -z "$lease" ] && [ -n "$RESOURCE_WORKTREE" ] && [ -n "$RESOURCE_SOCKET" ]; then
    lease=$(printf '%s' "$RESOURCE_REPORT" | jq -c '{version:1, project_slug:.project, story_id, repository_path:.repository, worktree_path:.worktree, branch, tmux:{socket_path}}')
  fi
  local -a args=(resources "$RESOURCE_ID" --json --window-name "$RESOURCE_WINDOW")
  [ -z "$lease" ] || args+=(--lease-json "$lease")
  [ -z "$RESOURCE_SOCKET" ] || args+=(--tmux-socket "$RESOURCE_SOCKET")
  result=$(story_cli "${args[@]}") || { printf '%s\n' "$result" >&2; return 1; }
  if ! printf '%s' "$result" | jq -e '.resources.status == "resolved" or .resources.status == "absent"' >/dev/null; then
    printf '%s\n' "$result" >&2
    return 1
  fi
  printf '%s' "$result" | jq -ce '.resources | select(.status == "resolved" or .status == "absent") | .pane | if . == null then {} else del(.dead) end'
}
