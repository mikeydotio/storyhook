# shellcheck shell=bash
# Existing-resource observations come from the native reader. Provider launch
# configuration must never choose what a lifecycle command inspects or removes.
RESOURCE_SOCKET=""
RESOURCE_LOGICAL_SOCKET=""
RESOURCE_PROTECTED=false
RESOURCE_REVIVIFY=null
RESOURCE_LOCATION_ONLY=false
RESOURCE_CALLER_TMUX="${TMUX:-}"
RESOURCE_CALLER_SOCKET="$RESOURCE_CALLER_TMUX"
RESOURCE_CALLER_SOCKET="${RESOURCE_CALLER_SOCKET%%,*}"
RESOURCE_CALLER_PANE="${TMUX_PANE:-}"

# All terminal operations after discovery use the selected server, including
# provider protocol helpers that otherwise inherit tmux's ambient target.
tmux() {
  if [ "$RESOURCE_PROTECTED" = true ]; then
    local -a flags=(-u)
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -S)
          [ "${2:-}" = "$RESOURCE_SOCKET" ] || { printf 'protected tmux selector changed\n' >&2; return 1; }
          shift 2 ;;
        -u|-N|-v) flags+=("$1"); shift ;;
        -*) printf 'unsupported protected tmux client flag: %s\n' "$1" >&2; return 1 ;;
        *) break ;;
      esac
    done
    command tmux -N -S "$RESOURCE_SOCKET" "${flags[@]}" "$@"
  elif [ -n "$RESOURCE_SOCKET" ] && [ "${1:-}" != -S ]; then
    command tmux -S "$RESOURCE_SOCKET" "$@"
  else
    command tmux "$@"
  fi
}

# Retain one generation through preflight and effects. Re-observation may
# confirm it but cannot silently move the operation to a successor.
prepare_tmux_target() {
  local mode="$1" selected="${2:-${RESOURCE_SOCKET:-}}" result endpoint logical protected
  RESOURCE_TARGET_ERROR=""
  if ! result=$(python3 "$STORY_PLUGIN_ROOT/lib/tmux-target.py" "$mode" "$selected" 2>&1); then
    RESOURCE_TARGET_ERROR="$result"
    return 1
  fi
  endpoint=$(printf '%s' "$result" | jq -er .endpoint) || return 1
  logical=$(printf '%s' "$result" | jq -er .socket) || return 1
  protected=$(printf '%s' "$result" | jq -r .protected) || return 1
  if [ "$RESOURCE_PROTECTED" = true ] && [ "$RESOURCE_LOGICAL_SOCKET" = "$logical" ] \
     && [ "$RESOURCE_SOCKET" != "$endpoint" ]; then
    RESOURCE_TARGET_ERROR='protected tmux generation changed during preflight'
    return 1
  fi
  RESOURCE_PROTECTED="$protected"
  RESOURCE_LOGICAL_SOCKET="$logical"
  RESOURCE_REVIVIFY=$(printf '%s' "$result" | jq -c 'if .protected then {logical_socket:.socket,origin_generation:.generation} else null end') || return 1
  if [ "$protected" = true ] || [ -n "$selected" ]; then
    RESOURCE_SOCKET="$endpoint"
    export TMUX="$endpoint,0,0"
  fi
}

# The current transport and its originating protection evidence travel together.
resource_cleanup_target() {
  jq -cn --arg socket "$1" --argjson provenance "$RESOURCE_REVIVIFY" \
    '{socket_path:$socket} + (if $provenance == null then {} else {revivify:$provenance} end)'
}

# An exact recheck handles only a concurrent creator of this session. Keep the
# creation diagnostic when no such session exists; never use new-session -A.
ensure_tmux_session() {
  local name="$1" directory="$2" diagnostic
  SESSION_CREATED=false
  SESSION_ERROR=""
  tmux has-session -t "=$name" 2>/dev/null && return 0
  if diagnostic=$(python3 "$STORY_PLUGIN_ROOT/lib/tmux-launch.py" new-session -d -s "$name" -c "$directory" 2>&1); then
    # Published to the dispatch receipt in story.sh.
    # shellcheck disable=SC2034
    SESSION_CREATED=true
    return 0
  fi
  case "$diagnostic" in
    *"duplicate session:"*) tmux has-session -t "=$name" 2>/dev/null && return 0 ;;
  esac
  # Published to the dispatch failure in story.sh.
  # shellcheck disable=SC2034
  SESSION_ERROR="$diagnostic"
  return 1
}

load_story_resources() {
  local id="$1" lease="${2:-}" location_only="${3:-false}" result state legacy_socket
  prepare_tmux_target inspect || refuse "resource-query-failed" "$RESOURCE_TARGET_ERROR"
  if [ "$RESOURCE_PROTECTED" = true ]; then
    legacy_socket="$RESOURCE_SOCKET"
  elif legacy_socket=$(tmux display-message -p '#{socket_path}' 2>/dev/null); then
    case "$legacy_socket" in /*) ;; *) legacy_socket="" ;; esac
  else
    legacy_socket=""
  fi
  if [ -n "$RESOURCE_CALLER_PANE" ]; then
    case "$RESOURCE_CALLER_SOCKET" in /*) ;; *) RESOURCE_CALLER_SOCKET="$legacy_socket" ;; esac
  fi
  local -a args=(resources "$id" --json)
  [ "$location_only" != true ] || args+=(--location-only)
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
  local observed_socket
  observed_socket=$(printf '%s' "$result" | jq -r '.resources.socket_path // empty')
  if [ -n "$observed_socket" ]; then
    prepare_tmux_target inspect "$observed_socket" \
      || refuse "resource-query-failed" "cannot bind resource socket: $RESOURCE_TARGET_ERROR"
  fi
  RESOURCE_PANE=$(printf '%s' "$result" | jq -r '.resources.pane.pane_id // empty')
  RESOURCE_REPORT=$(printf '%s' "$result" | jq -c '.resources')
  RESOURCE_LEASE="$lease"
  RESOURCE_LOCATION_ONLY="$location_only"
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

# RV-10 adoption can preserve a logical #{socket_path} on a private client.
# Publish the checked endpoint only when the observed alias agrees.
resource_socket_for_pane() {
  local observed
  observed=$(tmux display-message -p -t "$1" '#{socket_path}') || return 1
  if [ "$RESOURCE_PROTECTED" = true ]; then
    observed=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$observed") || return 1
    [ "$observed" = "$RESOURCE_SOCKET" ] || [ "$observed" = "$RESOURCE_LOGICAL_SOCKET" ] || return 1
    printf '%s' "$RESOURCE_SOCKET"
  else
    printf '%s' "$observed"
  fi
}

resource_is_self() {
  [ -n "$RESOURCE_CALLER_PANE" ] || return 1
  local caller_socket
  caller_socket=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$RESOURCE_CALLER_SOCKET") || return 1
  [ -z "$RESOURCE_SOCKET" ] || [ "$RESOURCE_SOCKET" = "$caller_socket" ] \
    || { [ "$RESOURCE_PROTECTED" = true ] && [ "$RESOURCE_LOGICAL_SOCKET" = "$caller_socket" ]; } || return 1
  [ "$1" != "$RESOURCE_CALLER_PANE" ] || return 0
  local caller_window target_window
  caller_window=$(tmux display-message -p -t "$RESOURCE_CALLER_PANE" '#{window_id}') \
    || { [ "$RESOURCE_PROTECTED" = true ]; return; }
  target_window=$(tmux display-message -p -t "$1" '#{window_id}') \
    || { [ "$RESOURCE_PROTECTED" = true ]; return; }
  [ -n "$caller_window" ] && [ "$caller_window" = "$target_window" ]
}

# Re-observe identity before a mutation. State/dirty/merge policy remains the
# caller's responsibility; this rejects a moved worktree or replaced pane.
revalidate_story_resources() {
  local identity before after pane_before pane_after
  identity='{repository,worktree,branch,socket_path,pane}'
  if [ "$RESOURCE_LOCATION_ONLY" = true ]; then
    identity='{location_only,repository,worktree,branch,socket_path,window_name}'
  fi
  before=$(printf '%s' "$RESOURCE_REPORT" | jq -c "$identity")
  pane_before=$(resource_find_pane) || refuse "resource-query-failed" "cannot recheck story window before mutation"
  load_story_resources "$RESOURCE_ID" "$RESOURCE_LEASE" "$RESOURCE_LOCATION_ONLY"
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
