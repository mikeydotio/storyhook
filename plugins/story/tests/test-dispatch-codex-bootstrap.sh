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
  FAKE_TMUX_CODEX_SENTINEL_AFTER_SUBMIT=1 FAKE_TMUX_CODEX_SENTINEL_MODE=identity \
  FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" \
  bash "$SCRIPT" dispatch "$id" --auto)
assert_eq "$(jqf "$out" .ok)" true "first-turn hook initializes and dispatch succeeds"
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits")" 2 "one initialization and one charter"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" "$id" "last submission is the story charter"

# Machine completion must belong to the intercepted turn and exact hook package.
source "$PLUGIN_ROOT/lib/codex-bootstrap.sh"
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
finish
