#!/usr/bin/env bash
# SH-859: production readiness, real process identity, external terminal boundary.
set -euo pipefail
root=$(cd "$(dirname "$0")/../../.." && pwd)
fixture=$(mktemp -d /tmp/story-trust-readiness.XXXXXX)
cleanup() {
  local pidfile
  for pidfile in "$fixture"/*/pane_pid; do
    [ ! -f "$pidfile" ] || kill "$(cat "$pidfile")" 2>/dev/null || true
  done
  rm -rf "$fixture"
}
trap cleanup EXIT
mkdir "$fixture/bin" "$fixture/worktree" "$fixture/other"
export WORKSPACE_TRUST_BASE_TMUX="$root/plugins/story/tests/fakes/tmux"
export FAKE_TMUX_SUPPRESS_SENTINEL=1 FAKE_TMUX_CAPTURE=marker FAKE_TMUX_PANE_LIFETIME=600
ln -s "$root/plugins/story/tests/fakes/workspace-trust-tmux" "$fixture/bin/tmux"
export PATH="$fixture/bin:$PATH"
# An inherited enable flag must not grant authority to generic readiness users.
export STARTUP_TRUST_ENABLED=true
source "$root/plugins/story/lib/session.sh"
[ "$STARTUP_TRUST_ENABLED" = false ] || { echo 'FAIL: inherited trust authority'; exit 1; }
STORY_PLUGIN_ROOT="$root/plugins/story"
READY_PATTERN='for shortcuts|for agents|mode on|to cycle'
READY_ATTEMPTS=18 READY_DELAY=0.1 READY_STABLE_POLLS=3
READY_FRAME_GLYPH='─' READY_PROMPT_GLYPH='›'

# Each case owns a fresh pane and consent state; no production function is mocked.
run_case() {
  local provider="$1" mode="$2" result=0 expected=true max_enters=1
  local STARTUP_TRUST_LIB="$STARTUP_TRUST_LIB"
  local case_name="$provider-$mode"
  export FAKE_TMUX_STATE="$fixture/$case_name" WORKSPACE_TRUST_PRETRUSTED=0
  mkdir "$FAKE_TMUX_STATE"
  [ "$mode" != trusted ] || export WORKSPACE_TRUST_PRETRUSTED=1
  local pane
  rm -f "$fixture/worktree/.claude/dispatch-sentinel.json"
  pane=$(tmux new-window -c "$fixture/worktree" -P -F '#{pane_id}' "$provider" \; set-window-option -t : remain-on-exit on)
  AGENT="$provider" READY_PROCESS_PATTERN="^$provider$" READY_LAUNCH_BIN="$provider"
  STARTUP_TRUST_ENABLED=true STARTUP_TRUST_PHASE=unseen
  STARTUP_TRUST_PANE="$pane" STARTUP_TRUST_PROVIDER="$provider"
  STARTUP_TRUST_FINGERPRINT='' STARTUP_TRUST_SINCE=''
  STARTUP_TRUST_WORKTREE="$fixture/worktree" STARTUP_TRUST_ROOT="$fixture/worktree"
  STARTUP_TRUST_PID=$(cat "$FAKE_TMUX_STATE/pane_pid")
  STARTUP_TRUST_START=$(python3 "$STORY_PLUGIN_ROOT/lib/agent_identity.py" capture "$STARTUP_TRUST_PID" | jq -r '.identity.start')
  # Keep the hook absent except for explicit successful/stale-witness cases.
  rm -f "$fixture/worktree/.claude/dispatch-sentinel.json"
  if [ "$provider" = claude ] && [ "$mode" != missing-hook ]; then
    mkdir -p "$fixture/worktree/.claude"
    printf '{"protocol_version":2,"plugin_root":"%s"}\n' "$STORY_PLUGIN_ROOT" > "$FAKE_TMUX_STATE/trust_held_sentinel"
    if [ "$mode" = trusted ] || [ "$mode" = stale-sentinel ]; then
      cp "$FAKE_TMUX_STATE/trust_held_sentinel" "$fixture/worktree/.claude/dispatch-sentinel.json"
    fi
  fi
  case "$mode" in
    ordinary|trusted|stale-sentinel) ;;
    negative) printf no > "$FAKE_TMUX_STATE/trust_selection" ;;
    delayed) printf 2 > "$FAKE_TMUX_STATE/trust_delay" ;;
    recheck-pid|recheck-selection|recheck-disappear)
      printf yes > "$FAKE_TMUX_STATE/trust_selection"
      printf '%s' "${mode#recheck-}" > "$FAKE_TMUX_STATE/trust_recheck_change"
      expected=false; max_enters=0 ;;
    wrong-path) printf '%s' "$fixture/other" > "$FAKE_TMUX_STATE/trust_display_path"; expected=false; max_enters=0 ;;
    wrong-cwd) printf '%s' "$fixture/worktree" > "$FAKE_TMUX_STATE/trust_display_path"; printf '%s' "$fixture/other" > "$FAKE_TMUX_STATE/pane_cwd"; expected=false; max_enters=0 ;;
    invalid-cwd) printf '%s' "$fixture/worktree" > "$FAKE_TMUX_STATE/trust_display_path"; printf /missing > "$FAKE_TMUX_STATE/pane_cwd"; expected=false; max_enters=0 ;;
    no-composer) touch "$FAKE_TMUX_STATE/trust_no_composer"; expected=false ;;
    classifier-failed) STARTUP_TRUST_LIB="$fixture/unavailable"; expected=false; max_enters=0 ;;
    wrong-pid) STARTUP_TRUST_PID=1; expected=false; max_enters=0 ;;
    wrong-start) STARTUP_TRUST_START=wrong; expected=false; max_enters=0 ;;
    wrong-pane) STARTUP_TRUST_PANE=%different; expected=false; max_enters=0 ;;
    wrong-provider) STARTUP_TRUST_PROVIDER=foreign; expected=false; max_enters=0 ;;
    changed) printf no > "$FAKE_TMUX_STATE/trust_selection"; touch "$FAKE_TMUX_STATE/trust_change_after_arrow"; expected=false; max_enters=0 ;;
    ignored-arrow) printf no > "$FAKE_TMUX_STATE/trust_selection"; touch "$FAKE_TMUX_STATE/trust_ignore_arrow"; expected=false; max_enters=0 ;;
    persistent) touch "$FAKE_TMUX_STATE/trust_keep_open"; expected=false ;;
    send-failed) touch "$FAKE_TMUX_STATE/trust_send_failure"; expected=false ;;
    unknown) printf 'Login required\n' > "$FAKE_TMUX_STATE/trust_screen"; expected=false; max_enters=0 ;;
    disabled) STARTUP_TRUST_ENABLED=false; expected=false; max_enters=0 ;;
    missing-hook) expected=false ;;
    wrong-hook) printf '{"protocol_version":2,"plugin_root":"%s"}\n' "$fixture/other" > "$FAKE_TMUX_STATE/trust_held_sentinel"; expected=false ;;
    *) echo "Unknown case $mode" >&2; exit 1 ;;
  esac
  if [ "$provider" = codex ]; then
    wait_ready "$pane" "$provider" > "$fixture/gate.stdout" || result=$?
  else
    wait_ready_sentinel "$pane" "$(cat "$FAKE_TMUX_STATE/pane_pid")" "$fixture/worktree" "$STORY_PLUGIN_ROOT" > "$fixture/gate.stdout" || result=$?
  fi
  [ ! -s "$fixture/gate.stdout" ] || { echo "FAIL: $case_name internal output leaked"; exit 1; }
  if { [ "$expected" = true ] && [ "$result" != 0 ]; } || { [ "$expected" = false ] && [ "$result" = 0 ]; }; then
    printf 'FAIL: %s readiness result=%s reason=%s phase=%s\n' "$case_name" "$result" "$WAIT_READY_REASON" "$STARTUP_TRUST_PHASE" >&2
    exit 1
  fi
  local enters=0
  [ ! -f "$FAKE_TMUX_STATE/trust_keys" ] || enters=$(grep -c '^Enter$' "$FAKE_TMUX_STATE/trust_keys" || true)
  [ "$enters" -le "$max_enters" ] || { echo "FAIL: $case_name unexpected confirmation"; exit 1; }
  [ ! -f "$FAKE_TMUX_STATE/trust_premature_key" ] || { echo "FAIL: $case_name premature input"; exit 1; }
  [ ! -f "$FAKE_TMUX_STATE/trust_rejected" ] || { echo "FAIL: $case_name negative confirmation"; exit 1; }
  if [ "$expected" = true ] && [ "$mode" != trusted ]; then
    [ -f "$FAKE_TMUX_STATE/trust_accepted" ] && [ "$enters" = 1 ] || { echo "FAIL: $case_name consent missing"; exit 1; }
  fi
  case "$mode" in
    wrong-path|wrong-cwd|invalid-cwd|wrong-pid|wrong-start|wrong-pane|wrong-provider|recheck-*|unknown|disabled|trusted)
      [ ! -f "$FAKE_TMUX_STATE/trust_keys" ] || { echo "FAIL: $case_name unexpected input"; exit 1; } ;;
    missing-hook) [ "$WAIT_READY_REASON" = no-sentinel ] || { echo 'FAIL: missing hook was hidden'; exit 1; } ;;
    wrong-hook) [ "$WAIT_READY_REASON" = hook-identity-mismatch ] || { echo 'FAIL: wrong hook was hidden'; exit 1; } ;;
    no-composer) [ "$WAIT_READY_REASON" = timeout ] || { echo "FAIL: consent hid readiness failure: $WAIT_READY_REASON"; exit 1; } ;;
    classifier-failed) [ "$WAIT_READY_REASON" = trust-classifier-failed ] || { echo 'FAIL: missing classifier has no diagnostic'; exit 1; } ;;
  esac
  kill "$(cat "$FAKE_TMUX_STATE/pane_pid")" 2>/dev/null || true
  rm "$FAKE_TMUX_STATE/pane_pid"
  printf 'PASS: %s (%s)\n' "$case_name" "$WAIT_READY_REASON"
}
if [ "${1:-}" = --trusted ]; then run_case codex trusted; exit; fi
if [ "${1:-}" = --case ]; then run_case "$2" "$3"; exit; fi
for provider in codex claude; do
  for mode in ordinary trusted negative delayed wrong-path wrong-cwd invalid-cwd wrong-pid wrong-start wrong-pane wrong-provider recheck-pid recheck-selection recheck-disappear changed ignored-arrow persistent send-failed classifier-failed unknown disabled; do
    run_case "$provider" "$mode"
  done
done
run_case codex no-composer
for mode in stale-sentinel missing-hook wrong-hook; do run_case claude "$mode"; done
