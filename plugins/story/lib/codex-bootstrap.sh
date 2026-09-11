#!/usr/bin/env bash
# Codex defers SessionStart until its first turn. A task-free turn invokes the
# hook, which stops it before model work; its transcript proves completion.
# Bootstrap metadata belongs to the worktree's private Git directory, never
# the shared checkout or provider installation.

codex_bootstrap_prepare() {
  local worktree="$1" private_dir
  private_dir=$(git -C "$worktree" rev-parse --absolute-git-dir) || return 1
  CODEX_BOOTSTRAP_TOKEN=$(uuidgen) || return 1
  CODEX_BOOTSTRAP_FILE="$private_dir/storyhook-codex-bootstrap-$CODEX_BOOTSTRAP_TOKEN.json"
  jq -n --arg attempt "$CODEX_BOOTSTRAP_TOKEN" --arg cwd "$(cd "$worktree" && pwd -P)" \
    '{version:1,phase:"pending",attempt:$attempt,cwd:$cwd}' > "$CODEX_BOOTSTRAP_FILE"
}

# A hook uses only the attempt its parent explicitly passed. A missing marker
# means this is an ordinary turn (including later compact/resume hooks).
codex_bootstrap_active() {
  [ -n "${STORYHOOK_CODEX_BOOTSTRAP:-}" ] && [ -f "$STORYHOOK_CODEX_BOOTSTRAP" ]
}

# Transform SessionStart's normal envelope only during initialization. The
# existing CLI remains the sole writer of the protocol-2 dispatch sentinel.
# A failed bootstrap still stops the turn, but publishes no successful receipt.
codex_bootstrap_hook_response() {
  local payload="$1" normal="$2" request="${STORYHOOK_CODEX_BOOTSTRAP:-}"
  if ! codex_bootstrap_active; then
    case "$normal" in "{"*) printf '%s' "$normal" ;; *) printf '{}' ;; esac
    return
  fi
  local attempt cwd session transcript turn receipt temp reason
  reason="Storyhook initialization failed; no work was authorized"
  attempt=$(jq -er 'select(.version == 1 and .phase == "pending") | .attempt | select(type == "string" and length > 0)' "$request") \
    && cwd=$(jq -er '.cwd' "$request") \
    && [ "$cwd" = "$(pwd -P)" ] \
    && session=$(printf '%s' "$payload" | jq -er 'select(.hook_event_name == "SessionStart" and .source == "startup") | .session_id | select(type == "string" and length > 0)') \
    && transcript=$(printf '%s' "$payload" | jq -er '.transcript_path | select(type == "string" and startswith("/"))') \
    && turn=$(jq -ser --arg session "$session" '
      select(any(.[]; .type == "session_meta" and .payload.id == $session)) |
      [.[] | select(.type == "event_msg" and .payload.type == "task_started")][-1].payload |
      select(.collaboration_mode_kind == "plan") | .turn_id | select(type == "string" and length > 0)' "$transcript") \
    && jq -e --arg root "$HOOK_PLUGIN_ROOT" --arg session "$session" \
      '.protocol_version == 2 and .plugin_root == $root and .session_id == $session' \
      "$cwd/.claude/dispatch-sentinel.json" >/dev/null \
    && receipt=$(jq -cn --arg attempt "$attempt" --arg root "$HOOK_PLUGIN_ROOT" \
      --arg session "$session" --arg transcript "$transcript" --arg turn "$turn" \
      '{version:1,phase:"stopped",attempt:$attempt,plugin_root:$root,session_id:$session,transcript:$transcript,turn_id:$turn}') \
    && temp=$(mktemp "$request.XXXXXX") \
    && printf '%s\n' "$receipt" > "$temp" && mv "$temp" "$request" \
    && reason="Storyhook initialization stopped: $attempt"
  jq -cn --arg reason "$reason" '{continue:false,stopReason:$reason}'
}

# Captured pane identity must still name the original live provider before
# each input boundary. A rendered footer and a PID frozen by remain-on-exit
# are not process evidence (SH-226/SH-231).
codex_bootstrap_pane_owned() {
  local pane="$1" pid="$2" current
  current=$(tmux display-message -p -t "$pane" '#{pane_pid}' 2>/dev/null) || current=""
  if [ -z "$pid" ] || [ "$current" != "$pid" ]; then
    WAIT_READY_REASON=pid-mismatch; return 1
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    WAIT_READY_REASON=pid-exited; return 1
  fi
  if ! pane_runs "$pane"; then
    WAIT_READY_REASON=wrong-process; return 1
  fi
}

# Machine-readable completion of THIS stopped turn, not a cleared input box.
# A newer turn, assistant output, or tools invalidate the bootstrap proof.
codex_bootstrap_completed() {
  local receipt="$1" root="$2" attempt="$3" transcript session turn
  jq -e --arg root "$root" --arg attempt "$attempt" \
    '.version == 1 and .phase == "stopped" and .plugin_root == $root and .attempt == $attempt' \
    "$receipt" >/dev/null 2>&1 || return 1
  transcript=$(jq -er '.transcript | select(type == "string" and startswith("/"))' "$receipt") || return 1
  session=$(jq -er '.session_id | select(type == "string" and length > 0)' "$receipt") || return 1
  turn=$(jq -er '.turn_id | select(type == "string" and length > 0)' "$receipt") || return 1
  jq -se --arg session "$session" --arg turn "$turn" '
    select(any(.[]; .type == "session_meta" and .payload.id == $session)) |
    ([to_entries[] | select(.value.type == "event_msg" and .value.payload.type == "task_started")][-1]) as $start |
    select($start.value.payload.turn_id == $turn and $start.value.payload.collaboration_mode_kind == "plan") |
    .[$start.key:] |
    any(.[]; .type == "event_msg" and .payload.type == "task_complete" and .payload.turn_id == $turn and .payload.last_agent_message == null) and
    all(.[]; .type != "response_item" or (.payload.type == "message" and (.payload.role == "user" or .payload.role == "developer")))
  ' "$transcript" >/dev/null 2>&1
}

codex_bootstrap_ready() {
  local pane="$1" pid="$2" worktree="$3" launch="$4" attempt=0
  CODEX_BOOTSTRAP_PHASE=not-started
  wait_ready "$pane" "$launch" || return 1
  codex_bootstrap_pane_owned "$pane" "$pid" || return 1
  ensure_provider_plan_mode "$pane" || { WAIT_READY_REASON=bootstrap-plan-unconfirmed; return 1; }
  codex_bootstrap_pane_owned "$pane" "$pid" || return 1
  # Do not retry an uncertain submission. Even a task-free primer is one turn.
  local SEND_RETRIES=0
  CODEX_BOOTSTRAP_PHASE=submitted
  send_prompt_confirmed "$pane" \
    "Storyhook initialization only. Do not use tools, ask questions, make a plan, or work on stories. Reply READY only if the startup hook does not stop this turn." \
    "story-bootstrap-$CODEX_BOOTSTRAP_TOKEN" \
    || { WAIT_READY_REASON=bootstrap-submit-unconfirmed; return 1; }
  wait_ready_sentinel "$pane" "$pid" "$worktree" "$STORY_PLUGIN_ROOT" || return 1
  WAIT_READY_REASON=bootstrap-incomplete
  while [ "$attempt" -lt "$READY_ATTEMPTS" ]; do
    codex_bootstrap_pane_owned "$pane" "$pid" || return 1
    if codex_bootstrap_completed "$CODEX_BOOTSTRAP_FILE" "$STORY_PLUGIN_ROOT" "$CODEX_BOOTSTRAP_TOKEN"; then
      # The terminal must also be able to receive the charter; transcript
      # completion and TUI readiness are separate observations.
      if [ "$(input_state "$pane")" = empty ] && ensure_provider_plan_mode "$pane"; then
        codex_bootstrap_pane_owned "$pane" "$pid" || return 1
        rm "$CODEX_BOOTSTRAP_FILE" || { WAIT_READY_REASON=bootstrap-cleanup-failed; return 1; }
        CODEX_BOOTSTRAP_PHASE=complete
        WAIT_READY_REASON=ok
        return 0
      fi
    fi
    sleep "$READY_DELAY"
    attempt=$((attempt + 1))
  done
  return 1
}
