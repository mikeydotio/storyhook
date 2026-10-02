#!/usr/bin/env bash
# Consent is enabled only by the managed dispatch scope, never inherited env.
STARTUP_TRUST_ENABLED=false
STARTUP_TRUST_PHASE=unseen
STARTUP_TRUST_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# startup_trust_observe <pane> — capture and classify; no terminal input.
startup_trust_observe() {
  local screen
  screen=$(tmux capture-pane -J -p -t "$1" 2>/dev/null) || { WAIT_READY_REASON=trust-capture-failed; return 1; }
  if ! STARTUP_TRUST_OBSERVATION=$(printf '%s' "$screen" | python3 -B "$STARTUP_TRUST_LIB/startup_trust.py" \
      "$STARTUP_TRUST_PROVIDER" "$STARTUP_TRUST_WORKTREE" "$STARTUP_TRUST_ROOT"); then
    WAIT_READY_REASON=$(printf '%s' "$STARTUP_TRUST_OBSERVATION" | jq -er '.reason // "trust-classifier-failed"' 2>/dev/null) || WAIT_READY_REASON=trust-classifier-failed
    return 1
  fi
}

# startup_trust_owned <pane> — bind each input to the original process and cwd.
startup_trust_owned() {
  local pane="$1" pid identity cwd
  [ "$pane" = "${STARTUP_TRUST_PANE:-}" ] && [ "$AGENT" = "$STARTUP_TRUST_PROVIDER" ] \
    || { WAIT_READY_REASON=trust-owner-mismatch; return 1; }
  pid=$(tmux display-message -p -t "$pane" '#{pane_pid}' 2>/dev/null) || pid=''
  [ -n "$pid" ] && [ "$pid" = "$STARTUP_TRUST_PID" ] \
    || { WAIT_READY_REASON=trust-pid-mismatch; return 1; }
  if ! identity=$(python3 -B "$STARTUP_TRUST_LIB/agent_identity.py" capture "$pid"); then
    WAIT_READY_REASON=trust-process-unavailable; return 1
  fi
  [ "$(printf '%s' "$identity" | jq -r '.identity.start')" = "$STARTUP_TRUST_START" ] \
    || { WAIT_READY_REASON=trust-process-changed; return 1; }
  # An expert readiness override cannot broaden consent to another provider.
  local READY_PROCESS_PATTERN="^${STARTUP_TRUST_PROVIDER}$"
  pane_runs "$pane" || { WAIT_READY_REASON=trust-wrong-process; return 1; }
  cwd=$(tmux display-message -p -t "$pane" '#{pane_current_path}' 2>/dev/null) || cwd=''
  python3 -B "$STARTUP_TRUST_LIB/startup_trust.py" same-path "$cwd" "$STARTUP_TRUST_WORKTREE" >/dev/null \
    || { WAIT_READY_REASON=trust-cwd-mismatch; return 1; }
}

# startup_trust_poll <pane> — 0 ordinary readiness, 2 pending, 1 refusal.
# The caller's existing attempt budget owns all waits, including confirmation.
startup_trust_poll() {
  [ "${STARTUP_TRUST_ENABLED:-false}" = true ] || return 0
  local pane="$1" fingerprint key now fresh_key
  startup_trust_observe "$pane" || return 1
  fingerprint=$(printf '%s' "$STARTUP_TRUST_OBSERVATION" | jq -r '.dialog.fingerprint // empty')
  if [ -z "$fingerprint" ]; then
    case "$STARTUP_TRUST_PHASE" in
      unseen|complete) return 0 ;;
      submitted) STARTUP_TRUST_PHASE=complete; WAIT_READY_REASON=timeout; return 0 ;;
      *) WAIT_READY_REASON=trust-dialog-disappeared; return 1 ;;
    esac
  fi
  key=$(printf '%s' "$STARTUP_TRUST_OBSERVATION" | jq -r '.dialog.key')
  now=$(printf '%s' "$STARTUP_TRUST_OBSERVATION" | jq -r '.now')
  if [ "$STARTUP_TRUST_PHASE" = unseen ]; then
    STARTUP_TRUST_FINGERPRINT="$fingerprint"
    STARTUP_TRUST_SINCE="$now"
    STARTUP_TRUST_PHASE=observing
  fi
  [ "$fingerprint" = "${STARTUP_TRUST_FINGERPRINT:-}" ] \
    || { WAIT_READY_REASON=trust-dialog-changed; return 1; }
  WAIT_READY_REASON=trust-timeout
  case "$STARTUP_TRUST_PHASE" in
    submitted) return 2 ;;
    complete) WAIT_READY_REASON=trust-dialog-returned; return 1 ;;
    selecting) [ "$key" = Enter ] || return 2 ;;
  esac
  printf '%s' "$STARTUP_TRUST_OBSERVATION" | jq -e --argjson since "$STARTUP_TRUST_SINCE" '.now - $since >= 1' >/dev/null \
    || return 2
  startup_trust_owned "$pane" || return 1
  startup_trust_observe "$pane" || return 1
  [ "$(printf '%s' "$STARTUP_TRUST_OBSERVATION" | jq -r '.dialog.fingerprint // empty')" = "$fingerprint" ] \
    || { WAIT_READY_REASON=trust-dialog-changed; return 1; }
  fresh_key=$(printf '%s' "$STARTUP_TRUST_OBSERVATION" | jq -r '.dialog.key')
  [ "$fresh_key" = "$key" ] || { WAIT_READY_REASON=trust-selection-changed; return 1; }
  startup_trust_owned "$pane" || return 1
  # Mark before the effect: an uncertain send can never be replayed.
  if [ "$key" = Enter ]; then STARTUP_TRUST_PHASE=submitted; else STARTUP_TRUST_PHASE=selecting; fi
  tmux send-keys -t "$pane" "$key" 2>/dev/null || { WAIT_READY_REASON=trust-input-failed; return 1; }
  return 2
}
