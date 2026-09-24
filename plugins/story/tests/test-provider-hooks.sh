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

# The same selector must reject unwired/ambiguous handlers and survive new
# neighbors. Mutate a fixture manifest, never the installed hook order.
selection_manifest="$FAKE_BIN/hooks.json"
expected_handler=$(jq -cn '{type:"command",command:"bash \"/plugin/hooks/stop-handoff.sh\"",timeout:15}')
jq -n --argjson handler "$expected_handler" \
  '{hooks:{Stop:[{matcher:"other",hooks:[$handler]},
    {matcher:"*",hooks:[{type:"command",command:"unrelated"},$handler]}]}}' >"$selection_manifest"
selected=$(manifest_hook Stop '*' stop-handoff.sh "$selection_manifest")
assert_eq "$selected" "$expected_handler" 'manifest selector ignores unrelated handlers and matchers'
jq '.hooks.Stop |= reverse | .hooks.Stop[].hooks |= reverse' \
  "$selection_manifest" >"$FAKE_BIN/reordered.json"
assert_eq "$(manifest_hook Stop '*' stop-handoff.sh "$FAKE_BIN/reordered.json")" \
  "$expected_handler" 'manifest selector survives group and handler reordering'
if manifest_hook Stop '*' missing.sh "$selection_manifest" >"$FAKE_BIN/selected" 2>"$FAKE_BIN/error"; then
  fail_test 'manifest selector must reject a missing handler'
fi
assert_contains "$(cat "$FAKE_BIN/error")" 'found 0' 'missing handler reports its match count'
assert_eq "$(cat "$FAKE_BIN/selected")" '' 'missing handler produces no executable selection'
jq '.hooks.Stop += [.hooks.Stop[1]]' "$selection_manifest" >"$FAKE_BIN/duplicate.json"
if manifest_hook Stop '*' stop-handoff.sh "$FAKE_BIN/duplicate.json" >"$FAKE_BIN/selected" 2>"$FAKE_BIN/error"; then
  fail_test 'manifest selector must reject duplicate handlers across groups'
fi
assert_contains "$(cat "$FAKE_BIN/error")" 'found 2' 'ambiguous handler reports its match count'
assert_eq "$(cat "$FAKE_BIN/selected")" '' 'ambiguous handler produces no executable selection'

repo=$(mktemp -d /tmp/story-test-provider-hooks-repo.XXXXXX)
_TMP_REPOS+=("$repo")
printf 'schema = 1\n[plugin]\nenabled = true\n' >"$repo/.storyhook.toml"

hook_command() {
  manifest_hook "$1" "$2" "$3" | jq -r '.command'
}

run_codex_hook() {
  local event="$1" matcher="$2" script="$3" payload="$4" command
  command=$(hook_command "$event" "$matcher" "$script") || return 1
  (cd "$repo" && printf '%s' "$payload" | env -u CLAUDE_PLUGIN_ROOT -u STORYHOOK_DISPATCH \
    PLUGIN_ROOT="$PLUGIN_ROOT" PATH="$FAKE_BIN:$PATH" bash -c "$command")
}

session_payload=$(printf '{"session_id":"codex-session-1","hook_event_name":"SessionStart","source":"startup","cwd":"%s"}' "$repo")
out=$(run_codex_hook SessionStart '*' session-start.sh "$session_payload")
assert_eq "$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName')" "SessionStart" \
  "SessionStart: valid context envelope"
assert_contains "$(cat "$STORY_HOOK_LOG")" "--deadline 3 session-start" \
  "SessionStart: bounded CLI invocation"
assert_eq "$(jq -r .session_id "$STORY_HOOK_STDIN")" "codex-session-1" \
  "SessionStart: provider session identity forwarded"
assert_eq "$(jq -r .cwd "$STORY_HOOK_STDIN")" "$repo" \
  "SessionStart: provider cwd forwarded"
assert_eq "$(jq -r .storyhook_plugin_root "$STORY_HOOK_STDIN")" "$PLUGIN_ROOT" \
  "SessionStart: exact executing plugin root added to the payload"

: >"$STORY_HOOK_LOG"
post_payload='{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"git pull --ff-only"}}'
out=$(run_codex_hook PostToolUse Bash post-git.sh "$post_payload")
assert_eq "$out" "{}" \
  "PostToolUse: successful background sync stays silent"
assert_contains "$(cat "$STORY_HOOK_LOG")" "--deadline 8 commit-sync --since 1h --quiet" \
  "PostToolUse: bounded sync invocation"

: >"$STORY_HOOK_LOG"
stop_payload='{"hook_event_name":"Stop","stop_hook_active":false}'
out=$(run_codex_hook Stop '*' stop-handoff.sh "$stop_payload")
assert_eq "$(printf '%s' "$out" | jq -r '.systemMessage')" "handoff ready" \
  "Stop: handoff becomes a system message"
assert_contains "$(cat "$STORY_HOOK_LOG")" "--deadline 13 handoff --since 4h" \
  "Stop: bounded handoff invocation"

# Claude's compatibility variable remains a valid fallback for the same root.
command=$(hook_command SessionStart '*' session-start.sh) || exit 1
out=$(cd "$repo" && printf '%s' "$session_payload" | env -u PLUGIN_ROOT -u STORYHOOK_DISPATCH \
  CLAUDE_PLUGIN_ROOT="$PLUGIN_ROOT" PATH="$FAKE_BIN:$PATH" bash -c "$command")
assert_eq "$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName')" "SessionStart" \
  "Claude root fallback: same manifest command works"

# SH-758: Claude sets CLAUDE_PLUGIN_ROOT for every hook but never PLUGIN_ROOT,
# so a PLUGIN_ROOT the session inherited (a tmux server started from Codex) is
# ambient and names another host's copy. Codex sets both. Every manifest
# command must therefore run the copy under CLAUDE_PLUGIN_ROOT, and a host that
# sets only Codex's documented PLUGIN_ROOT must still resolve. Marker copies
# stand in for both installs; their paths contain spaces on purpose.
claude_root="$FAKE_BIN/claude root"
codex_root="$FAKE_BIN/codex root"
export ROOT_LOG="$FAKE_BIN/roots"
manifest_commands=$(jq -r '.hooks[][] | .hooks[] | select(.type == "command") | .command' \
  "$PLUGIN_ROOT/hooks/hooks.json")
assert_eq "$(printf '%s\n' "$manifest_commands" | grep -c .)" "10" "every manifest hook command is covered"
while IFS= read -r script; do
  for root in "$claude_root" "$codex_root"; do
    mkdir -p "$root/hooks"
    printf '#!/usr/bin/env bash\nprintf "%%s\\n" %q >>"$ROOT_LOG"\n' "$root" >"$root/hooks/$script"
  done
done < <(printf '%s\n' "$manifest_commands" | sed -E 's|.*/hooks/([^"]+)".*|\1|' | sort -u)

ran_root() {
  : >"$ROOT_LOG"
  (cd "$repo" && env -u PLUGIN_ROOT -u CLAUDE_PLUGIN_ROOT "$@" bash -c "$command" </dev/null >/dev/null 2>&1)
  cat "$ROOT_LOG"
}
while IFS= read -r command; do
  assert_eq "$(ran_root PLUGIN_ROOT="$codex_root" CLAUDE_PLUGIN_ROOT="$claude_root")" "$claude_root" \
    "root precedence: a host-set CLAUDE_PLUGIN_ROOT wins over an inherited PLUGIN_ROOT: $command"
  assert_eq "$(ran_root PLUGIN_ROOT="$codex_root")" "$codex_root" \
    "root precedence: a PLUGIN_ROOT-only host still resolves: $command"
  assert_eq "$(ran_root CLAUDE_PLUGIN_ROOT="$claude_root")" "$claude_root" \
    "root precedence: a CLAUDE_PLUGIN_ROOT-only host resolves: $command"
done <<<"$manifest_commands"

# The dispatch readiness gate compares the sentinel's recorded root with the
# helper's own. Under an inherited foreign PLUGIN_ROOT, the real SessionStart
# hook must record the Claude root, not the other host's copy.
command=$(hook_command SessionStart '*' session-start.sh) || exit 1
: >"$ROOT_LOG"
out=$(cd "$repo" && printf '%s' "$session_payload" | env -u STORYHOOK_DISPATCH \
  PLUGIN_ROOT="$codex_root" CLAUDE_PLUGIN_ROOT="$PLUGIN_ROOT" PATH="$FAKE_BIN:$PATH" bash -c "$command")
assert_eq "$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName')" "SessionStart" \
  "inherited PLUGIN_ROOT: the real SessionStart hook ran"
assert_eq "$(jq -r .storyhook_plugin_root "$STORY_HOOK_STDIN")" "$PLUGIN_ROOT" \
  "inherited PLUGIN_ROOT: the sentinel records the Claude root"
assert_eq "$(cat "$ROOT_LOG")" "" "inherited PLUGIN_ROOT: the other host's copy never ran"

finish
