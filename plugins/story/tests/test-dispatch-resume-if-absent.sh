#!/usr/bin/env bash
# SH-850: `dispatch --resume --if-absent` is the resume that never replaces a
# live agent. Plain `--resume` respawns a surviving pane with `-k`, whatever
# runs in it, so a dashboard click on a working story killed its agent. With
# --if-absent a live pane refuses `agent-live` before any change, a dead pane
# is respawned WITHOUT -k (tmux itself refuses a pane that came alive), a
# missing window is recreated, and a provider process working in the worktree
# outside the recorded window -- a tmux server whose socket path was taken
# over, or an agent started by hand -- refuses too.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# The daemon's envelope, as test-notify-redispatch.sh documents it.
daemon_dispatch() {
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

forget_logs() {
  rm -f "$FAKE_TMUX_STATE/new_window_args.log" "$FAKE_TMUX_STATE/respawn_pane_args.log"
}

# --- the fake models tmux's own refusal ---------------------------------------
repo=$(mk_story_repo RIA)
id=$(new_story "$repo" "Its agent is still working")
out=$(daemon_dispatch "$repo" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" "true" "live: the original dispatch succeeds"
pane=$(jqf "$out" .pane)
worktree=$(jqf "$out" .worktree_path)
export FAKE_TMUX_PANES="$id	1	$pane"
if "$FAKE_TMUX_DIR/tmux" respawn-pane -t "$pane" "true" 2>/dev/null; then
  fail_test "fake: a no-k respawn of a live pane is refused, as tmux refuses it"
fi
forget_logs

# --- a live pane: refused, nothing changed -----------------------------------
out=$(daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "false" "live: the guarded resume is refused"
assert_eq "$(jqf "$out" .reason)" "agent-live" "live: the refusal names the live agent"
assert_eq "$(jqf "$out" .live_pane)" "$pane" "live: the refusal names the pane"
assert_contains "$(jqf "$out" .display)" "Nothing was changed" "live: the refusal says nothing changed"
[ ! -f "$FAKE_TMUX_STATE/respawn_pane_args.log" ] || fail_test "live: the pane was respawned"
[ ! -f "$FAKE_TMUX_STATE/new_window_args.log" ] || fail_test "live: a window was opened"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r .story.story.state)" "in-progress" \
  "live: the story keeps its claim"

# Control: plain --resume still replaces the pane with -k (SH-523's contract).
out=$(daemon_dispatch "$repo" dispatch "$id" --resume)
assert_eq "$(jqf "$out" .ok)" "true" "unguarded: plain --resume still respawns"
assert_contains "$(cat "$FAKE_TMUX_STATE/respawn_pane_args.log")" "-k -c $worktree" \
  "unguarded: plain --resume kills with -k"
forget_logs

# --- a dead pane: respawned in place, without -k -----------------------------
kill_pane
out=$(daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "true" "dead: the guarded resume succeeds"
assert_eq "$(jqf "$out" .resumed)" "true" "dead: it is a resume"
assert_eq "$(jqf "$out" .window_reused)" "true" "dead: the same pane is respawned"
assert_eq "$(jqf "$out" .pane)" "$pane" "dead: same pane id"
respawned=$(cat "$FAKE_TMUX_STATE/respawn_pane_args.log")
assert_contains "$respawned" "-c $worktree" "dead: respawn-pane over the worktree"
case "$respawned" in
  *"-k "*) fail_test "dead: a guarded respawn never passes -k (got: $respawned)" ;;
esac
assert_contains "$respawned" "rm -f -- " "dead: the stale witness is removed only once tmux accepts the pane"
[ ! -f "$FAKE_TMUX_STATE/new_window_args.log" ] || fail_test "dead: a competing window was opened"
forget_logs

# --- a missing window: recreated under the same name --------------------------
window_id=$("$FAKE_TMUX_DIR/tmux" display-message -p -t "$pane" '#{window_id}')
"$FAKE_TMUX_DIR/tmux" kill-window -t "$window_id"
unset FAKE_TMUX_PANES

# A provider process still works in the worktree, outside any recorded window.
# `sleep` stands in for it: tmux names a real Claude by its version string, and
# the test cannot run one, so the census is told `sleep` is a provider.
( cd "$worktree" && exec sleep 120 ) &
occupant=$!
out=$(STORY_READY_PROCESS_PATTERN='^(claude|node|codex|sleep)$' \
  daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "false" "occupied: a live agent outside the window refuses the resume"
assert_eq "$(jqf "$out" .reason)" "agent-live" "occupied: the refusal names the live agent"
assert_eq "$(jqf "$out" '.occupants[0].pid')" "$occupant" "occupied: the refusal names the process"
assert_contains "$(jqf "$out" .display)" "pid $occupant sleep" "occupied: the display names it too"
[ ! -f "$FAKE_TMUX_STATE/new_window_args.log" ] || fail_test "occupied: a window was opened"
kill "$occupant" 2>/dev/null || true
wait "$occupant" 2>/dev/null || true

out=$(daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "true" "gone: the guarded resume recreates the window"
assert_eq "$(jqf "$out" .window_reused)" "false" "gone: no pane survived to respawn"
assert_eq "$(jqf "$out" .worktree_reused)" "true" "gone: the worktree is reused"
assert_contains "$(cat "$FAKE_TMUX_STATE/new_window_args.log")" "-n $id " \
  "gone: new-window carries the story's own name"

# --- flag validation ----------------------------------------------------------
out=$(daemon_dispatch "$repo" dispatch "$id" --if-absent)
assert_eq "$(jqf "$out" .ok)" "false" "validation: --if-absent without --resume is refused"
assert_contains "$(jqf "$out" .display)" "--if-absent requires --resume" "validation: names --resume"
out=$(daemon_dispatch "$repo" dispatch "$id" --resume --if-absent --if-absent)
assert_eq "$(jqf "$out" .ok)" "false" "validation: a repeated --if-absent is refused"
assert_contains "$(jqf "$out" .display)" "--if-absent may be specified only once" "validation: names the repeat"
out=$(daemon_dispatch "$repo" dispatch "$id" --resume --if-absent --require-absent --continuation-file=/nonexistent)
assert_eq "$(jqf "$out" .ok)" "false" "validation: --if-absent with --require-absent is refused"
assert_contains "$(jqf "$out" .display)" "cannot be combined with --require-absent" "validation: names the conflict"
out=$(daemon_dispatch "$repo" dispatch --next --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "false" "validation: --if-absent needs a named story"

finish
