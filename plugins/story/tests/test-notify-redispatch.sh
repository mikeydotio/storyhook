#!/usr/bin/env bash
# SH-650: the helper verbs the verifier composes when a returned story's agent
# is gone. `notify` refuses by a name that means absence; `dispatch --resume
# --auto` -- invoked the way the daemon invokes it, from outside tmux with a
# target session -- respawns the SAME pane in the SAME window over the SAME
# worktree and hands the agent the charter with the resume clause; a second
# `notify` then delivers the diagnosis verbatim. Then the window-gone variant:
# the same verbs recreate the window under the same name.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# The daemon's envelope for a control verb: no $TMUX/$TMUX_PANE (the dispatch
# allowlist strips them), STORY_TARGET_SESSION and STORY_CREATE_SESSION set the
# way run_shell_dispatch sets them, and no STORY_AGENT.
verifier() {
  local repo="$1"
  shift
  (
    cd "$repo" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        STORY_TARGET_SESSION="$(slug_for "$repo")" STORY_CREATE_SESSION=1 \
        STORY_COUNCIL=off STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" "$@" 2>&1
  )
}

kill_pane() {
  local pid
  pid=$(cat "$FAKE_TMUX_STATE/pane_pid")
  kill -9 "$pid" 2>/dev/null || true
  while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done
}

diagnosis="CENTRAL VERIFICATION RED — merge tree \`deadbeef\` failed \`make test\`.
Fix the existing PR, run new and impacted tests, push, then move the story back to verifying.

one regression"

# --- dead pane: respawned in place -------------------------------------------
repo=$(mk_story_repo RDP)
id=$(new_story "$repo" "Returned to a dead pane")
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_COUNCIL=off STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id" --auto 2>&1
)
assert_eq "$(jqf "$out" .ok)" "true" "dead pane: the original autonomous dispatch succeeds"
worktree=$(jqf "$out" .worktree_path)
pane=$(jqf "$out" .pane)
window=$(jqf "$out" .window_name)
export FAKE_TMUX_PANES="$id	1	$pane"
# The original dispatch's own window log is not evidence about the resume.
rm -f "$FAKE_TMUX_STATE/new_window_args.log"
# The agent finished and stopped: its pane is a corpse under remain-on-exit.
kill_pane

out=$(verifier "$repo" notify "$id" "$diagnosis")
assert_eq "$(jqf "$out" .ok)" "false" "dead pane: notify refuses"
assert_eq "$(jqf "$out" .reason)" "pane-dead" "dead pane: the refusal names absence"

out=$(verifier "$repo" dispatch "$id" --resume --auto)
assert_eq "$(jqf "$out" .ok)" "true" "dead pane: the resume re-dispatch succeeds from outside tmux"
assert_eq "$(jqf "$out" .resumed)" "true" "dead pane: it is a resume, not a fresh dispatch"
assert_eq "$(jqf "$out" .reused_claim)" "true" "dead pane: the in-progress claim is reused"
assert_eq "$(jqf "$out" .claim_transitioned)" "false" "dead pane: no state transition"
assert_eq "$(jqf "$out" .window_reused)" "true" "dead pane: the same pane is respawned"
assert_eq "$(jqf "$out" .pane)" "$pane" "dead pane: same pane id"
assert_eq "$(jqf "$out" .window_name)" "$window" "dead pane: same window name"
assert_eq "$(jqf "$out" .worktree_path)" "$worktree" "dead pane: same worktree"
assert_eq "$(jqf "$out" .auto)" "true" "dead pane: the autonomous charter"
assert_contains "$(cat "$FAKE_TMUX_STATE/respawn_pane_args.log")" "-k -c $worktree" \
  "dead pane: respawn-pane -k over the worktree"
assert_contains "$(cat "$FAKE_TMUX_STATE/respawn_pane_args.log")" "-t $pane " \
  "dead pane: respawn targets the surviving pane"
[ ! -f "$FAKE_TMUX_STATE/new_window_args.log" ] \
  || fail_test "dead pane: a competing window was opened"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" "resuming work already started" \
  "dead pane: the charter carries the resume clause"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" "story move $id verifying" \
  "dead pane: the charter still hands the story back to the verifier"

out=$(verifier "$repo" notify "$id" "$diagnosis")
assert_eq "$(jqf "$out" .ok)" "true" "dead pane: the diagnosis is delivered to the respawned agent"
assert_eq "$(cat "$FAKE_TMUX_STATE/submitted")" "$diagnosis" \
  "dead pane: the diagnosis is submitted verbatim as one prompt"

# --- window gone: recreated under the same name --------------------------------
export FAKE_TMUX_STATE
FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")
repo=$(mk_story_repo RWG)
id=$(new_story "$repo" "Returned to a missing window")
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_COUNCIL=off STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id" --auto 2>&1
)
assert_eq "$(jqf "$out" .ok)" "true" "window gone: the original dispatch succeeds"
worktree=$(jqf "$out" .worktree_path)
window=$(jqf "$out" .window_name)
# Someone killed the window; nothing named after the story is listed.
unset FAKE_TMUX_PANES
rm -f "$FAKE_TMUX_STATE/new_window_args.log"

out=$(verifier "$repo" notify "$id" "$diagnosis")
assert_eq "$(jqf "$out" .ok)" "false" "window gone: notify refuses"
assert_eq "$(jqf "$out" .reason)" "pane-unavailable" "window gone: the refusal names absence"

out=$(verifier "$repo" dispatch "$id" --resume --auto)
assert_eq "$(jqf "$out" .ok)" "true" "window gone: the resume re-dispatch succeeds"
assert_eq "$(jqf "$out" .resumed)" "true" "window gone: it is a resume"
assert_eq "$(jqf "$out" .worktree_reused)" "true" "window gone: the worktree is reused"
assert_eq "$(jqf "$out" .window_reused)" "false" "window gone: no pane survived to respawn"
assert_eq "$(jqf "$out" .window_name)" "$window" "window gone: the window is recreated under the same name"
assert_eq "$(jqf "$out" .worktree_path)" "$worktree" "window gone: same worktree"
assert_contains "$(cat "$FAKE_TMUX_STATE/new_window_args.log")" "-n $window " \
  "window gone: new-window carries the story's own name"
assert_contains "$(cat "$FAKE_TMUX_STATE/new_window_args.log")" "-t $(slug_for "$repo"): " \
  "window gone: the window opens in the project's session, not the daemon's (absent) one"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" "resuming work already started" \
  "window gone: the charter carries the resume clause"

export FAKE_TMUX_PANES="$id	1	$(jqf "$out" .pane)"
out=$(verifier "$repo" notify "$id" "$diagnosis")
assert_eq "$(jqf "$out" .ok)" "true" "window gone: the diagnosis is delivered to the new window"
assert_eq "$(cat "$FAKE_TMUX_STATE/submitted")" "$diagnosis" \
  "window gone: the diagnosis is submitted verbatim"

finish
