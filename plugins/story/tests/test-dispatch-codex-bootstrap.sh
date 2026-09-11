#!/usr/bin/env bash
# SH-675: SessionStart cannot precede Codex's first prompt.
source "$(dirname "$0")/lib.sh"
FAKE_BIN=$(mktemp -d /tmp/story-test-bootstrap-bin.XXXXXX)
_TMP_REPOS+=("$FAKE_BIN")
printf '#!/bin/sh\nexit 0\n' >"$FAKE_BIN/codex"
chmod +x "$FAKE_BIN/codex"
export FAKE_TMUX_STATE
FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-bootstrap-tmux.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")
repo=$(mk_story_repo CBT)
id=$(new_story "$repo" "First-turn initialization")
out=$(cd "$repo" && PATH="$FAKE_BIN:$TESTS_DIR/fakes:$PATH" \
  TMUX=fake TMUX_PANE=%0 STORY_AGENT=codex \
  STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=2 STORY_CONFIRM_DELAY=0 \
  STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
  FAKE_TMUX_CODEX_SENTINEL_MODE=identity \
  FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" \
  bash "$SCRIPT" dispatch "$id" --auto)
assert_eq "$(jqf "$out" .ok)" true "first-turn hook initializes and dispatch succeeds"
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits")" 2 "one initialization and one charter"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" "$id" "last submission is the story charter"

# Machine completion must belong to the intercepted turn and exact hook package.
source "$PLUGIN_ROOT/lib/codex-bootstrap.sh"
# The required Python runtime supplies the nonce on platforms without uuidgen.
uuidgen() { return 127; }
if codex_bootstrap_prepare "$repo"; then
  [[ "$CODEX_BOOTSTRAP_TOKEN" =~ ^[0-9a-f]{32}$ ]] || fail_test "nonce must be 128 random bits in hex"
else
  fail_test "initialization requires an undeclared uuidgen dependency"
fi
unset -f uuidgen
receipt="$FAKE_TMUX_STATE/receipt.json"
transcript="$FAKE_TMUX_STATE/receipt-transcript.jsonl"
jq -n --arg path "$transcript" --arg root "$PLUGIN_ROOT" \
  '{version:1,phase:"stopped",attempt:"expected",plugin_root:$root,session_id:"s",turn_id:"t",transcript:$path}' > "$receipt"
valid_transcript='{"type":"session_meta","payload":{"id":"s"}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"t","collaboration_mode_kind":"plan"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t","last_agent_message":null}}'
printf '%s\n' "$valid_transcript" > "$transcript"
codex_bootstrap_completed "$receipt" "$PLUGIN_ROOT" expected || fail_test "matching stopped turn must complete"
for transform in \
  '.attempt="stale"' '.plugin_root="/different"' '.version=0' '.phase="pending"' \
  '.session_id="other"' '.turn_id="other"' '.transcript="relative"'; do
  jq "$transform" "$receipt" > "$receipt.bad"
  if codex_bootstrap_completed "$receipt.bad" "$PLUGIN_ROOT" expected; then
    fail_test "invalid receipt accepted: $transform"
  fi
done
for event in \
  '{"type":"event_msg","payload":{"type":"task_started","turn_id":"new","collaboration_mode_kind":"plan"}}' \
  '{"type":"response_item","payload":{"type":"function_call","name":"exec_command"}}' \
  '{"type":"response_item","payload":{"type":"message","role":"assistant"}}' \
  '{'; do
  printf '%s\n%s\n' "$valid_transcript" "$event" > "$transcript"
  if codex_bootstrap_completed "$receipt" "$PLUGIN_ROOT" expected; then
    fail_test "unexpected work or partial transcript accepted: $event"
  fi
done
printf '%s\n' "$valid_transcript" | head -2 > "$transcript"
if codex_bootstrap_completed "$receipt" "$PLUGIN_ROOT" expected; then
  fail_test "task_started alone is not turn completion"
fi
printf '%s\n' "$valid_transcript" | sed 's/"plan"/"default"/' > "$transcript"
if codex_bootstrap_completed "$receipt" "$PLUGIN_ROOT" expected; then
  fail_test "default-mode bootstrap cannot prove Plan-first initialization"
fi
# Stop cannot create an ordinary session handoff while bootstrap is active.
printf '{}' > "$receipt"
hook_out=$(STORYHOOK_CODEX_BOOTSTRAP="$receipt" bash "$PLUGIN_ROOT/hooks/stop-handoff.sh")
assert_eq "$hook_out" '{}' "initialization Stop suppresses ordinary handoff context"

FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-bootstrap-incomplete.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")
id=$(new_story "$repo" "Incomplete initialization")
out=$(cd "$repo" && PATH="$FAKE_BIN:$TESTS_DIR/fakes:$PATH" \
  TMUX=fake TMUX_PANE=%0 STORY_AGENT=codex \
  STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=2 STORY_CONFIRM_DELAY=0 \
  STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
  FAKE_TMUX_CODEX_SENTINEL_MODE=identity FAKE_TMUX_BOOTSTRAP_INCOMPLETE=1 \
  FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" \
  bash "$SCRIPT" dispatch "$id" --auto)
assert_eq "$(jqf "$out" .ok)" false "a hook receipt alone cannot deliver the charter"
assert_eq "$(jqf "$out" .wait_ready_reason)" bootstrap-incomplete "missing completion is diagnosed"
assert_eq "$(jqf "$out" .bootstrap_phase)" submitted "refusal identifies the submitted initialization phase"
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits")" 1 "incomplete initialization is never resubmitted"
finish
