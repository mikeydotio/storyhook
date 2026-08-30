#!/usr/bin/env bash
# SH-460 -- what the full-auto hook decides, and everything it must not.
#
# The hook is the whole of decision D6 in docs/spec/full-auto-engine.md: a lane
# nobody is watching gets its plan approved and its questions refused at exact
# provider events. Claude's PermissionRequest event triggers a bounded watcher
# of only its own plan-review pane; there is no global/continuous pane monitor.
#
# Every case fires the command `hooks.json` actually ships, resolved through the
# provider root variables an installed plugin resolves, the way
# test-provider-hooks.sh does: the WIRING is as much under test as the script,
# because a correct script nothing invokes decides nothing.
#
# Two properties here look redundant and are not. The hook re-reads `tool_name`
# from the payload even though the matcher already selected it, so a Bash
# payload gets no decision even when it arrives through the ExitPlanMode door.
# That keeps the script correct under a wildcard matcher, which is what a future
# host with no exact-name matching would force -- and it is the difference
# between deciding on the payload and deciding on having been invoked.
source "$(dirname "$0")/lib.sh"

MANIFEST="$PLUGIN_ROOT/hooks/hooks.json"
HOOK="$PLUGIN_ROOT/hooks/full-auto.sh"
LANE_STORY="SH-460"

repo=$(mktemp -d /tmp/story-test-fullauto-hook.XXXXXX)
_TMP_REPOS+=("$repo")

hook_command() {
  jq -r --arg e "$1" --arg m "$2" \
    '.hooks[$e][] | select(.matcher == $m) | .hooks[0].command' "$MANIFEST"
}

# fire <matcher> <payload> -- run the manifest's command for <matcher> with a
# literal payload on stdin, inheriting the caller's STORYHOOK_FULL_AUTO.
fire() {
  local matcher="$1" payload="$2" command
  command=$(hook_command PreToolUse "$matcher")
  if [ -z "$command" ] || [ "$command" = null ]; then
    fail_test "full-auto: hooks.json declares no PreToolUse matcher '$matcher'"
    return 0
  fi
  (cd "$repo" && printf '%s' "$payload" | env -u CLAUDE_PLUGIN_ROOT \
    PLUGIN_ROOT="$PLUGIN_ROOT" bash -c "$command")
}

fire_permission() {
  local payload="$1" command
  command=$(hook_command PermissionRequest ExitPlanMode)
  if [ -z "$command" ] || [ "$command" = null ]; then
    fail_test "full-auto: hooks.json declares no PermissionRequest matcher 'ExitPlanMode'"
    return 0
  fi
  (cd "$repo" && printf '%s' "$payload" | env -u CLAUDE_PLUGIN_ROOT \
    PLUGIN_ROOT="$PLUGIN_ROOT" bash -c "$command")
}

claude_payload() {
  printf '{"session_id":"lane-1","transcript_path":"/dev/null","cwd":"%s",' "$repo"
  printf '"permission_mode":"plan","hook_event_name":"PreToolUse","tool_name":"%s",' "$1"
  printf '"tool_use_id":"call_1","tool_input":%s}' "$2"
}

# The Codex envelope SH-459 measured against CLI 0.149.0: the same event and
# decision vocabulary, plus turn_id and model, under a different tool name.
codex_payload() {
  printf '{"session_id":"lane-1","turn_id":"turn-1","transcript_path":"/dev/null",'
  printf '"cwd":"%s","model":"gpt-5-codex","permission_mode":"auto",' "$repo"
  printf '"hook_event_name":"PreToolUse","tool_name":"request_user_input",'
  printf '"tool_use_id":"call_1","tool_input":{"questions":[{"question":"which?"}]}}'
}

permission_payload() {
  printf '{"session_id":"lane-1","transcript_path":"/dev/null","cwd":"%s",' "$repo"
  printf '"permission_mode":"plan","hook_event_name":"PermissionRequest",'
  printf '"tool_name":"%s","tool_input":%s}' "$1" "$2"
}

decision_of() { printf '%s' "$1" | jq -r '.hookSpecificOutput.permissionDecision // "none"'; }
reason_of() { printf '%s' "$1" | jq -r '.hookSpecificOutput.permissionDecisionReason // ""'; }
event_of() { printf '%s' "$1" | jq -r '.hookSpecificOutput.hookEventName // ""'; }

export STORYHOOK_FULL_AUTO="$LANE_STORY"

# --- the plan exit is approved, with no human at the prompt -----------------
out=$(fire ExitPlanMode "$(claude_payload ExitPlanMode '{"plan":"do the thing"}')"); status=$?
assert_eq "$status" "0" "ExitPlanMode: exits 0"
assert_eq "$(event_of "$out")" "PreToolUse" "ExitPlanMode: answers as PreToolUse"
assert_eq "$(decision_of "$out")" "allow" "ExitPlanMode: the plan is approved"
case "$(reason_of "$out")" in
  *"$LANE_STORY"*) ;;
  *) fail_test "ExitPlanMode: the approval reason does not name the lane's story" ;;
esac

# Claude Code 2.1.251 has two approval boundaries: PreToolUse authorizes the
# ExitPlanMode tool, then PermissionRequest presents the separate plan-review
# pane. The latter returns no provider decision; it starts a bounded watcher
# that presses Return only when the exact default-selected Auto option is on
# the hook's own pane.
TMUX_FIXTURE=$(mktemp -d /tmp/story-test-fullauto-tmux.XXXXXX)
_TMP_REPOS+=("$TMUX_FIXTURE")
cat >"$TMUX_FIXTURE/tmux" <<'MOCKTMUX'
#!/usr/bin/env bash
case "${1:-}" in
  run-shell)
    bash -c "${*: -1}"
    ;;
  capture-pane)
    if [ "${FULL_AUTO_TMUX_SCREEN:-ready}" != ready ]; then
      printf 'Provider changed this prompt\n1. Continue\n'
    elif [ "${FULL_AUTO_TMUX_PROVIDER:-claude}" = codex ]; then
      printf 'Implement this plan?\n› 1. Yes, implement this plan\n  2. Yes, clear context and implement\n  3. No, stay in Plan mode\n'
    else
      printf 'Ready to code?\n❯ 1. Yes, and use auto mode\n  2. Yes, manually approve edits\n'
    fi
    ;;
  send-keys)
    printf '%s\n' "$*" >>"$FULL_AUTO_TMUX_LOG"
    ;;
  *) exit 1 ;;
esac
MOCKTMUX
chmod +x "$TMUX_FIXTURE/tmux"
export FULL_AUTO_TMUX_LOG="$TMUX_FIXTURE/send-keys.log"
: >"$FULL_AUTO_TMUX_LOG"

out=$(PATH="$TMUX_FIXTURE:$PATH" TMUX_PANE=%4242 \
  fire_permission "$(permission_payload ExitPlanMode '{"plan":"do the thing"}')")
assert_eq "$out" "{}" "PermissionRequest: returns an empty provider directive"
for _ in $(seq 1 40); do
  [ -s "$FULL_AUTO_TMUX_LOG" ] && break
  sleep 0.05
done
assert_eq "$(cat "$FULL_AUTO_TMUX_LOG")" "send-keys -t %4242 Enter" \
  "PermissionRequest: accepts the exact Auto plan pane with Return"

# A changed prompt must fail closed: one bounded probe, no input. Likewise an
# invalid pane id cannot become a tmux target.
: >"$FULL_AUTO_TMUX_LOG"
PATH="$TMUX_FIXTURE:$PATH" FULL_AUTO_TMUX_SCREEN=changed \
  bash "$HOOK" --approve-claude-plan %4242 1
assert_eq "$(cat "$FULL_AUTO_TMUX_LOG")" "" \
  "PermissionRequest: changed UI text receives no input"
PATH="$TMUX_FIXTURE:$PATH" bash "$HOOK" --approve-claude-plan 'not-a-pane' 1
assert_eq "$(cat "$FULL_AUTO_TMUX_LOG")" "" \
  "PermissionRequest: an invalid pane id receives no input"

# Codex has no pre-dialog hook event. Dispatch starts the sibling pane-lifetime
# watcher after confirming Plan mode; the same exact-match rule permits one
# Return and a changed UI receives none.
: >"$FULL_AUTO_TMUX_LOG"
PATH="$TMUX_FIXTURE:$PATH" FULL_AUTO_TMUX_PROVIDER=codex \
  bash "$HOOK" --approve-codex-plan %4242 1
assert_eq "$(cat "$FULL_AUTO_TMUX_LOG")" "send-keys -t %4242 Enter" \
  "Codex watcher: accepts the exact selected plan pane with Return"
: >"$FULL_AUTO_TMUX_LOG"
PATH="$TMUX_FIXTURE:$PATH" FULL_AUTO_TMUX_PROVIDER=codex FULL_AUTO_TMUX_SCREEN=changed \
  bash "$HOOK" --approve-codex-plan %4242 1
assert_eq "$(cat "$FULL_AUTO_TMUX_LOG")" "" \
  "Codex watcher: changed UI text receives no input"

# SH-511: ordinary `dispatch --auto` activates the same native decision surface
# through its own marker, without claiming to be an engine lane.
unset STORYHOOK_FULL_AUTO
export STORYHOOK_AUTO="SH-511"
out=$(fire ExitPlanMode "$(claude_payload ExitPlanMode '{"plan":"do the thing"}')")
assert_eq "$(decision_of "$out")" "allow" "STORYHOOK_AUTO: the plan is approved"
assert_contains "$(reason_of "$out")" "SH-511" \
  "STORYHOOK_AUTO: the approval reason names the autonomous story"
out=$(fire request_user_input "$(codex_payload)")
assert_eq "$(decision_of "$out")" "deny" "STORYHOOK_AUTO: Codex questions are refused"
assert_contains "$(reason_of "$out")" "SH-511" \
  "STORYHOOK_AUTO: question feedback names the autonomous story"
unset STORYHOOK_AUTO
export STORYHOOK_FULL_AUTO="$LANE_STORY"

# --- the question is refused, on both hosts, with the same instruction ------
for probe in "AskUserQuestion:$(claude_payload AskUserQuestion '{"questions":[{"question":"which?"}]}')" \
             "request_user_input:$(codex_payload)"; do
  matcher="${probe%%:*}"; body="${probe#*:}"
  out=$(fire "$matcher" "$body"); status=$?
  assert_eq "$status" "0" "$matcher: exits 0"
  assert_eq "$(event_of "$out")" "PreToolUse" "$matcher: answers as PreToolUse"
  assert_eq "$(decision_of "$out")" "deny" "$matcher: the question is refused"
  reason=$(reason_of "$out")
  # The feedback is the whole point of denying rather than failing: the model
  # reads it and has to be able to act on it without a person. Each span is a
  # separate obligation, so each is named separately.
  for needle in "unattended" "council-vote" "do not stall" "$LANE_STORY" \
                "before you resume the work"; do
    assert_contains "$reason" "$needle" "$matcher: the denial feedback carries '$needle'"
  done
done

# --- everything else gets no decision, even through a question's own door ---
#
# SH-355: a hook that decides must decide only what it was built to. Fired with
# a Bash payload through the ExitPlanMode matcher, the hook must read the
# payload and decline -- if it answered on the strength of having been invoked
# it would be approving arbitrary tool calls the moment a host stopped matching
# on exact names.
out=$(fire ExitPlanMode "$(claude_payload Bash '{"command":"rm -rf /"}')"); status=$?
assert_eq "$status" "0" "a Bash payload: exits 0"
assert_eq "$out" "{}" "a Bash payload through the plan door gets no decision"

for tool in Edit Write Read Task; do
  out=$(fire AskUserQuestion "$(claude_payload "$tool" '{}')")
  assert_eq "$out" "{}" "$tool: no decision"
done

# --- a non-PreToolUse envelope is not this hook's to answer -----------------
out=$(fire AskUserQuestion \
  '{"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion","tool_input":{}}')
assert_eq "$out" "{}" "a PostToolUse envelope gets no decision"

# --- a marker that is not a story id still enforces, and invents nothing ----
#
# SH-461 exports this variable; nothing yet pins its spelling but this hook.
# A launcher that sets it to `1` must still get an unattended lane, and the
# feedback must not name a story that does not exist.
STORYHOOK_FULL_AUTO=1
out=$(fire AskUserQuestion "$(claude_payload AskUserQuestion '{"questions":[]}')")
assert_eq "$(decision_of "$out")" "deny" "a non-id marker still refuses the question"
reason=$(reason_of "$out")
assert_contains "$reason" "council-vote" "a non-id marker still gets the full instruction"
case "$reason" in
  *"comment on 1 "*|*"on 1."*) fail_test "a non-id marker was echoed into the feedback as a story id" ;;
esac
STORYHOOK_FULL_AUTO="$LANE_STORY"

# --- an unreadable payload decides nothing, and says so by saying nothing ---
#
# Fail OPEN, deliberately (SH-306's shape, stated in the spec): a hook that
# cannot tell which tool it is must not decide. The lane then asks a question
# nobody answers and the engine's stall ceiling quarantines it -- detected and
# reported, which is the bar, rather than a hook guessing `deny` at a plan exit.
for broken in 'not json at all' '' '{"hook_event_name":"PreToolUse"' '[]' 'null'; do
  out=$(fire AskUserQuestion "$broken"); status=$?
  assert_eq "$status" "0" "a malformed payload exits 0"
  assert_eq "$out" "{}" "a malformed payload gets no decision"
done

# --- the plugin kill switch is deliberately NOT a second off switch ---------
#
# `[plugin] enabled = false` turns off the SESSION hooks -- context injection
# and handoff, both of which are conveniences. Unattendedness is not: a switch
# that could silently turn a lane back into an attended one re-opens the exact
# failure this hook exists to close, in the one configuration nobody would think
# to check. The marker is the whole of the activation.
printf 'schema = 1\n[plugin]\nenabled = false\n' >"$repo/.storyhook.toml"
out=$(fire AskUserQuestion "$(claude_payload AskUserQuestion '{"questions":[]}')")
assert_eq "$(decision_of "$out")" "deny" \
  "enabled = false does not disarm the lane's unattendedness"
rm -f "$repo/.storyhook.toml"

# --- the manifest's own budget stays a bounded one -------------------------
#
# Not a style check: PreToolUse fails OPEN at its timeout on both hosts, so this
# number is how long a lane waits before the hole opens. It is asserted here as
# well as in tests/hook_budgets.rs because this suite is the one a shell-only
# change runs.
for event_matcher in PreToolUse:ExitPlanMode PreToolUse:AskUserQuestion \
                     PreToolUse:request_user_input PermissionRequest:ExitPlanMode; do
  event="${event_matcher%%:*}"
  matcher="${event_matcher#*:}"
  budget=$(jq -r --arg m "$matcher" \
    --arg e "$event" '.hooks[$e][] | select(.matcher == $m) | .hooks[0].timeout' "$MANIFEST")
  case "$budget" in
    ''|null|0) fail_test "full-auto: $event '$matcher' declares no timeout" ;;
    *) [ "$budget" -le 30 ] || fail_test "full-auto: $event '$matcher' timeout ${budget}s is longer than a lane should wait" ;;
  esac
done

finish
