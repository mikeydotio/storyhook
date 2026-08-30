#!/usr/bin/env bash
# SH-460 -- the FULL-AUTO-INERT invariant.
#
# `full-auto.sh` is the only hook in this plugin that can DECIDE or TYPE: it
# approves a plan exit and refuses a question. Both are correct inside a lane and
# both are hostile everywhere else -- a hook that silently auto-approved plans
# in a developer's own session would be a far worse defect than the stall it
# exists to prevent. So the marker environment variable is the whole of its
# activation, and its absence has to be a tested property rather than a claim,
# in the shape test-charter-inert.sh already tests the charter's.
#
# WHY THE MANIFEST IS THE DOOR. Every case here runs the hook through the exact
# command `hooks.json` ships, resolved the way an installed plugin resolves it,
# because "inert" is a property of what the provider actually invokes -- not of
# a script somebody ran by hand with a different environment.
source "$(dirname "$0")/lib.sh"

HOOK="$PLUGIN_ROOT/hooks/full-auto.sh"
MANIFEST="$PLUGIN_ROOT/hooks/hooks.json"

repo=$(mktemp -d /tmp/story-test-fullauto-inert.XXXXXX)
_TMP_REPOS+=("$repo")

INERT_TMUX=$(mktemp -d /tmp/story-test-fullauto-inert-tmux.XXXXXX)
_TMP_REPOS+=("$INERT_TMUX")
cat >"$INERT_TMUX/tmux" <<'INERTTMUX'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$FULL_AUTO_INERT_TMUX_LOG"
INERTTMUX
chmod +x "$INERT_TMUX/tmux"
export FULL_AUTO_INERT_TMUX_LOG="$INERT_TMUX/calls.log"
: >"$FULL_AUTO_INERT_TMUX_LOG"

# hook_command <matcher> -- the command hooks.json ships for one PreToolUse
# matcher. Looked up BY MATCHER, never by position: this event carries three
# entries and an index would quietly hand a case somebody else's wiring.
hook_command() {
  jq -r --arg e "${2:-PreToolUse}" --arg m "$1" \
    '.hooks[$e][] | select(.matcher == $m) | .hooks[0].command' "$MANIFEST"
}

# payload <tool> -- a provider-shaped PreToolUse envelope for <tool>.
payload() {
  printf '{"session_id":"inert-1","transcript_path":"/dev/null","cwd":"%s",' "$repo"
  printf '"hook_event_name":"PreToolUse","tool_name":"%s",' "$1"
  printf '"tool_use_id":"call_1","tool_input":{"plan":"do the thing"}}'
}

# run <matcher> <tool> -- fire the manifest's command for <matcher> with a
# <tool> payload, in whatever autonomous markers the caller has arranged.
run() {
  local command
  command=$(hook_command "$1")
  [ -n "$command" ] && [ "$command" != null ] \
    || fail_test "full-auto-inert: hooks.json declares no PreToolUse matcher '$1'"
  (cd "$repo" && payload "$2" | env -u CLAUDE_PLUGIN_ROOT PLUGIN_ROOT="$PLUGIN_ROOT" \
    bash -c "$command")
}

run_permission() {
  local command
  command=$(hook_command ExitPlanMode PermissionRequest)
  [ -n "$command" ] && [ "$command" != null ] \
    || fail_test "full-auto-inert: hooks.json declares no PermissionRequest matcher 'ExitPlanMode'"
  (cd "$repo" && printf '{"hook_event_name":"PermissionRequest","tool_name":"ExitPlanMode","tool_input":{"plan":"do it"}}' \
    | env -u CLAUDE_PLUGIN_ROOT PLUGIN_ROOT="$PLUGIN_ROOT" TMUX_PANE=%4242 bash -c "$command")
}

TOOLS="ExitPlanMode:ExitPlanMode AskUserQuestion:AskUserQuestion request_user_input:request_user_input"

# --- unset: the marker is absent, as it is in every session but a lane -------
unset STORYHOOK_FULL_AUTO
unset STORYHOOK_AUTO
for pair in $TOOLS; do
  matcher="${pair%%:*}"; tool="${pair#*:}"
  out=$(run "$matcher" "$tool"); status=$?
  assert_eq "$status" "0" "inert/unset: $tool exits 0"
  assert_eq "$out" "{}" "inert/unset: $tool emits an empty directive"
done
assert_eq "$(PATH="$INERT_TMUX:$PATH" run_permission)" "{}" \
  "inert/unset: PermissionRequest emits an empty directive and types nothing"
assert_eq "$(cat "$FULL_AUTO_INERT_TMUX_LOG")" "" \
  "inert/unset: PermissionRequest never contacts tmux"
PATH="$INERT_TMUX:$PATH" bash "$HOOK" --approve-codex-plan %4242 1
assert_eq "$(cat "$FULL_AUTO_INERT_TMUX_LOG")" "" \
  "inert/unset: the Codex watcher never contacts tmux"

# --- set but EMPTY: an export with no value is not a lane -------------------
#
# The distinction matters because a launcher that computes the marker and gets
# nothing would otherwise activate the hook with an empty story id -- deciding
# on behalf of a lane that does not exist.
export STORYHOOK_FULL_AUTO=""
export STORYHOOK_AUTO=""
for pair in $TOOLS; do
  matcher="${pair%%:*}"; tool="${pair#*:}"
  out=$(run "$matcher" "$tool"); status=$?
  assert_eq "$status" "0" "inert/empty: $tool exits 0"
  assert_eq "$out" "{}" "inert/empty: $tool emits an empty directive"
done
assert_eq "$(PATH="$INERT_TMUX:$PATH" run_permission)" "{}" \
  "inert/empty: PermissionRequest emits an empty directive and types nothing"
assert_eq "$(cat "$FULL_AUTO_INERT_TMUX_LOG")" "" \
  "inert/empty: PermissionRequest never contacts tmux"
PATH="$INERT_TMUX:$PATH" bash "$HOOK" --approve-codex-plan %4242 1
assert_eq "$(cat "$FULL_AUTO_INERT_TMUX_LOG")" "" \
  "inert/empty: the Codex watcher never contacts tmux"
unset STORYHOOK_FULL_AUTO
unset STORYHOOK_AUTO

# --- no decision word reaches the transcript in either case -----------------
#
# `{}` is already asserted above; this is the narrower claim that no partial
# decision leaks by some other route -- a permissionDecision key with no
# envelope around it would satisfy neither assertion, but only this one names
# the reason it matters.
out=$(run AskUserQuestion AskUserQuestion)
case "$out" in
  *permissionDecision*) fail_test "full-auto-inert: an inert hook emitted a permissionDecision" ;;
esac

# --- the inert path starts no interpreter -----------------------------------
#
# Not a micro-optimisation dressed as a test. Every session but a lane takes this
# path, on every plan exit and every question a human is there to answer, and the
# hook's own marker check is duplicated on purpose -- once in bash before the
# interpreter, once inside it (SH-365's two-mechanism shape). The consequence is
# that inverting or deleting the OUTER check changes no observable answer: python
# refuses on the same marker and emits the same `{}`. Measured, not assumed --
# inverting the bash gate leaves every assertion above green.
#
# So the outer gate is observed directly, by putting a decoy python3 on PATH and
# asserting nothing reached it. Without this, the cost the gate exists to avoid
# could be reintroduced permanently and silently.
DECOY=$(mktemp -d /tmp/story-test-fullauto-decoy.XXXXXX)
_TMP_REPOS+=("$DECOY")
cat >"$DECOY/python3" <<'DECOYPY'
#!/usr/bin/env bash
printf 'reached\n' >>"$FULL_AUTO_DECOY_LOG"
printf '{}'
DECOYPY
chmod +x "$DECOY/python3"
export FULL_AUTO_DECOY_LOG="$DECOY/reached"
: >"$FULL_AUTO_DECOY_LOG"

unset STORYHOOK_FULL_AUTO
unset STORYHOOK_AUTO
for pair in $TOOLS; do
  matcher="${pair%%:*}"; tool="${pair#*:}"
  command=$(hook_command "$matcher")
  out=$(cd "$repo" && payload "$tool" | env -u CLAUDE_PLUGIN_ROOT \
    PLUGIN_ROOT="$PLUGIN_ROOT" PATH="$DECOY:$PATH" bash -c "$command")
  assert_eq "$out" "{}" "inert/no-interpreter: $tool still emits an empty directive"
done
assert_eq "$(wc -l <"$FULL_AUTO_DECOY_LOG" | tr -d ' ')" "0" \
  "the inert path started an interpreter -- the marker gate above it is not doing its job"

# ...and the decoy is reachable, so the assertion above cannot pass because the
# fixture is broken rather than because the gate works (SH-364: a fixture that
# lies to a gate agrees with it and proves nothing).
export STORYHOOK_FULL_AUTO=SH-460
command=$(hook_command AskUserQuestion)
(cd "$repo" && payload AskUserQuestion | env -u CLAUDE_PLUGIN_ROOT \
  PLUGIN_ROOT="$PLUGIN_ROOT" PATH="$DECOY:$PATH" bash -c "$command") >/dev/null
assert_eq "$(wc -l <"$FULL_AUTO_DECOY_LOG" | tr -d ' ')" "1" \
  "the decoy python3 was never reachable -- the no-interpreter assertion above proves nothing"
unset STORYHOOK_FULL_AUTO
unset STORYHOOK_AUTO

# --- inertness must never be buyable by deleting the decisions --------------
#
# Exactly test-charter-inert.sh's reasoning: a hook that decides nothing at all
# passes every assertion above and is useless. These are the load-bearing spans.
[ -f "$HOOK" ] || fail_test "full-auto-inert: $HOOK does not exist"
source_text=$(cat "$HOOK" 2>/dev/null || true)
for needle in ExitPlanMode PermissionRequest AskUserQuestion request_user_input \
              approve-codex-plan 'Implement this plan?' \
              '"allow"' '"deny"' STORYHOOK_AUTO STORYHOOK_FULL_AUTO council-vote; do
  case "$source_text" in
    *"$needle"*) ;;
    *) fail_test "full-auto-inert: the hook no longer names '$needle' -- inertness must not be bought by removing the decisions" ;;
  esac
done

# ...nor by unwiring it. An inert hook and an unwired one are indistinguishable
# from outside, and only one of them is correct.
for matcher in ExitPlanMode AskUserQuestion request_user_input; do
  command=$(hook_command "$matcher")
  case "$command" in
    *hooks/full-auto.sh*) ;;
    *) fail_test "full-auto-inert: hooks.json's PreToolUse '$matcher' entry no longer runs full-auto.sh" ;;
  esac
done
command=$(hook_command ExitPlanMode PermissionRequest)
case "$command" in
  *hooks/full-auto.sh*) ;;
  *) fail_test "full-auto-inert: hooks.json's PermissionRequest ExitPlanMode entry no longer runs full-auto.sh" ;;
esac

finish
