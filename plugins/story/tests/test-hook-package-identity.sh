#!/usr/bin/env bash
# SH-736: plugin spelling must not change stopped-turn authority.
source "$(dirname "$0")/lib.sh"
source "$PLUGIN_ROOT/lib/codex-bootstrap.sh"
root=$(mktemp -d /tmp/story-test-root-identity.XXXXXX)
_TMP_REPOS+=("$root")
mkdir "$root/package with spaces" "$root/other"
ln -s "$root/package with spaces" "$root/alias"
ln -s "$root/missing" "$root/broken"
transcript="$root/transcript.jsonl"
printf '%s\n' \
  '{"type":"session_meta","payload":{"id":"s"}}' \
  '{"type":"event_msg","payload":{"type":"task_started","turn_id":"t","collaboration_mode_kind":"plan"}}' \
  '{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t","last_agent_message":null}}' > "$transcript"
jq -n --arg root "$root/alias" --arg transcript "$transcript" \
  '{version:1,phase:"stopped",attempt:"a",plugin_root:$root,session_id:"s",turn_id:"t",transcript:$transcript}' > "$root/receipt"
codex_bootstrap_completed "$root/receipt" "$root/package with spaces" a \
  || fail_test "symlink-equivalent package must preserve stopped-turn proof"
for wrong in "$root/other" "$root/broken" "$root/missing" "relative" ""; do
  if codex_bootstrap_completed "$root/receipt" "$wrong" a; then
    fail_test "invalid package accepted: $wrong"
  fi
done
# No canonicalization can excuse an attempt, session or turn mismatch.
for change in '.attempt="wrong"' '.session_id="wrong"' '.turn_id="wrong"'; do
  jq "$change" "$root/receipt" > "$root/bad"
  if codex_bootstrap_completed "$root/bad" "$root/package with spaces" a; then
    fail_test "canonical package excused $change"
  fi
done
for invalid in '{' '{}{}' '[]' 'old CLI error'; do
  normal=$(STORYHOOK_CODEX_BOOTSTRAP= codex_bootstrap_hook_response '{}' "$invalid")
  assert_eq "$normal" '{}' "ordinary hook output must be exactly one JSON object"
done
finish
