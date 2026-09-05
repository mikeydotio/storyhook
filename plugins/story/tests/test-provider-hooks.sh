#!/usr/bin/env bash
# The installed hook manifest executes from either provider root and accepts
# the current documented payload shapes for SessionStart, PostToolUse(Bash),
# and Stop. A fake `story` observes argv/stdin; no daemon is needed.
source "$(dirname "$0")/lib.sh"

FAKE_BIN=$(mktemp -d /tmp/story-test-provider-hooks.XXXXXX)
_TMP_REPOS+=("$FAKE_BIN")
export STORY_HOOK_LOG="$FAKE_BIN/calls"
export STORY_HOOK_STDIN="$FAKE_BIN/stdin"

cat >"$FAKE_BIN/story" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STORY_HOOK_LOG"
payload=$(cat)
printf '%s' "$payload" >"$STORY_HOOK_STDIN"
case " $* " in
  *" session-start "*) printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"Storyhook context"}}' ;;
  *" commit-sync "*) printf 'synced' ;;
  *" handoff "*) printf 'handoff ready' ;;
  *) printf '{}' ;;
esac
FAKE
chmod +x "$FAKE_BIN/story"

repo=$(mktemp -d /tmp/story-test-provider-hooks-repo.XXXXXX)
_TMP_REPOS+=("$repo")
printf 'schema = 1\n[plugin]\nenabled = true\n' >"$repo/.storyhook.toml"

hook_command() {
  jq -r --arg event "$1" '.hooks[$event][0].hooks[0].command' "$PLUGIN_ROOT/hooks/hooks.json"
}

run_codex_hook() {
  local event="$1" payload="$2" command
  command=$(hook_command "$event")
  (cd "$repo" && printf '%s' "$payload" | env -u CLAUDE_PLUGIN_ROOT -u STORYHOOK_DISPATCH \
    PLUGIN_ROOT="$PLUGIN_ROOT" PATH="$FAKE_BIN:$PATH" bash -c "$command")
}

session_payload=$(printf '{"session_id":"codex-session-1","hook_event_name":"SessionStart","source":"startup","cwd":"%s"}' "$repo")
out=$(run_codex_hook SessionStart "$session_payload")
assert_eq "$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName')" "SessionStart" \
  "SessionStart: valid context envelope"
assert_contains "$(cat "$STORY_HOOK_LOG")" "--deadline 3 session-start" \
  "SessionStart: bounded CLI invocation"
assert_eq "$(cat "$STORY_HOOK_STDIN")" "$session_payload" \
  "SessionStart: provider payload forwarded intact"

: >"$STORY_HOOK_LOG"
post_payload='{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"git pull --ff-only"}}'
out=$(run_codex_hook PostToolUse "$post_payload")
assert_eq "$(printf '%s' "$out" | jq -r '.systemMessage')" "[storyhook] Git sync: synced" \
  "PostToolUse: Bash command payload triggers sync"
assert_contains "$(cat "$STORY_HOOK_LOG")" "--deadline 8 commit-sync --since 1h --quiet" \
  "PostToolUse: bounded sync invocation"

: >"$STORY_HOOK_LOG"
stop_payload='{"hook_event_name":"Stop","stop_hook_active":false}'
out=$(run_codex_hook Stop "$stop_payload")
assert_eq "$(printf '%s' "$out" | jq -r '.systemMessage')" "handoff ready" \
  "Stop: handoff becomes a system message"
assert_contains "$(cat "$STORY_HOOK_LOG")" "--deadline 13 handoff --since 4h" \
  "Stop: bounded handoff invocation"

# Claude's compatibility variable remains a valid fallback for the same root.
command=$(hook_command SessionStart)
out=$(cd "$repo" && printf '%s' "$session_payload" | env -u PLUGIN_ROOT -u STORYHOOK_DISPATCH \
  CLAUDE_PLUGIN_ROOT="$PLUGIN_ROOT" PATH="$FAKE_BIN:$PATH" bash -c "$command")
assert_eq "$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName')" "SessionStart" \
  "Claude root fallback: same manifest command works"

finish
