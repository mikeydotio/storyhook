#!/usr/bin/env bash
# The same persisted project toggle controls hooks in an already-open session.
source "$(dirname "$0")/lib.sh"
repo=$(mk_story_repo MANUAL)
cd "$repo" || exit 1
payload='{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[]}}'
fire() {
  printf '%s' "$payload" | STORYHOOK_AUTO=MANUAL-1 bash "$PLUGIN_ROOT/hooks/full-auto.sh"
}
assert_eq "$(fire | jq -r '.hookSpecificOutput.permissionDecision')" deny 'enabled project retains unattended question policy'
story project settings set automations.enabled false >/dev/null
assert_eq "$(fire)" '{}' 'disabled project imposes no unattended question policy'
assert_eq "$(printf '{}' | story session-start)" '{}' 'disabled project injects no session context'
story project settings set automations.enabled true >/dev/null
assert_eq "$(fire | jq -r '.hookSpecificOutput.permissionDecision')" deny 're-enabled project restores hook policy'
finish
