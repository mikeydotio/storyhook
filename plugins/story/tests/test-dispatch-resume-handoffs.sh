#!/usr/bin/env bash
# SH-850 council C2 and C4: a resume replaces the lost session, and it says so.
#
# C2 -- the replacement must be a NEW session. The lost session's readiness
# witness is read before launch, as an unverified diagnostic: it names what
# was replaced in the result and in the dispatch comment, and a launch that
# reports the same session id is refused, not handed the charter.
#
# C4 -- a resume retires the story's context-handoff chain through
# `story internal supersede-continuations` before it launches: the lost
# receiving session can never acknowledge it. While a handoff is attempting
# delivery the resume refuses, launching nothing, because the continuation
# monitor owns that gap. The store semantics are pinned by
# tests/continuation_replacement.rs; this pins the helper's wiring.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# The daemon's envelope, as test-notify-redispatch.sh documents it.
daemon_dispatch() {
  local repo="$1"
  shift
  (
    cd "$repo" \
      && PATH="${HANDOFF_PATH_PREFIX:-}$FAKE_TMUX_DIR:$PATH" \
        STORY_TARGET_SESSION="$(slug_for "$repo")" STORY_CREATE_SESSION=1 \
        STORY_COUNCIL=off STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" "$@" 2>&1
  )
}

lose_window() {
  local pane="$1" window_id
  window_id=$("$FAKE_TMUX_DIR/tmux" display-message -p -t "$pane" '#{window_id}')
  "$FAKE_TMUX_DIR/tmux" kill-window -t "$window_id"
  unset FAKE_TMUX_PANES
  rm -f "$FAKE_TMUX_STATE/new_window_args.log" "$FAKE_TMUX_STATE/respawn_pane_args.log"
}

comments() {
  (cd "$1" && story show "$2" --json) | jq -r '[.story.story.comments[].text] | join("\n")'
}

# --- C2: the lost session is named, and a relaunch of it is refused -----------
repo=$(mk_story_repo RHS)
id=$(new_story "$repo" "Resumed after its session was lost")
out=$(daemon_dispatch "$repo" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" "true" "session: the original dispatch succeeds"
worktree=$(jqf "$out" .worktree_path)
lost=$(jq -r .session_id "$worktree/.claude/dispatch-sentinel.json")
[ -n "$lost" ] && [ "$lost" != null ] || fail_test "session: the original session published its id"
lose_window "$(jqf "$out" .pane)"

out=$(FAKE_TMUX_SESSION_ID="$lost" daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "false" "session: a launch reporting the lost id is refused"
assert_eq "$(jqf "$out" .reason)" "resume-session-reused" "session: the refusal names the reuse"
assert_eq "$(jqf "$out" .previous_session.session_id)" "$lost" "session: the refusal names the lost id"
assert_contains "$(jqf "$out" .display)" "No story charter was delivered" "session: no charter reached it"

out=$(daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "true" "session: a fresh session resumes the story"
assert_eq "$(jqf "$out" .previous_session.session_id)" "$lost" "session: the result names the lost session"
fresh=$(jq -r .session_id "$worktree/.claude/dispatch-sentinel.json")
[ "$fresh" != "$lost" ] || fail_test "session: the replacement reports a new id"
assert_contains "$(comments "$repo" "$id")" "It replaces the lost session $lost (unverified" \
  "session: the dispatch comment names the lost session"

# --- C4: the handoff chain is retired, or the resume waits for its monitor ----
repo=$(mk_story_repo RHC)
id=$(new_story "$repo" "Its handoff chain outlived its session")
out=$(daemon_dispatch "$repo" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" "true" "chain: the original dispatch succeeds"
lose_window "$(jqf "$out" .pane)"

shim=$(mktemp -d /tmp/story-test-handoffs.XXXXXX)
_TMP_REPOS+=("$shim")
real_story=$(command -v story)
cat >"$shim/story" <<SHIM
#!/usr/bin/env bash
case " \$* " in
  *' internal supersede-continuations '*)
    printf '%s\n' "\$*" >>"$shim/calls"
    case "\$HANDOFF_MODE" in
      attempting) printf '{"protocol_version":1,"project":"%s","story_id":"%s","superseded":[],"attempting":["req-9"]}\n' "\$HANDOFF_PROJECT" "\$HANDOFF_STORY" ;;
      superseded) printf '{"protocol_version":1,"project":"%s","story_id":"%s","superseded":["req-7"],"attempting":[]}\n' "\$HANDOFF_PROJECT" "\$HANDOFF_STORY" ;;
      *) exec "$real_story" "\$@" ;;
    esac
    exit 0 ;;
esac
exec "$real_story" "\$@"
SHIM
chmod +x "$shim/story"
export HANDOFF_PROJECT HANDOFF_STORY
HANDOFF_PROJECT=$(slug_for "$repo")
HANDOFF_STORY="$id"

out=$(HANDOFF_MODE=attempting HANDOFF_PATH_PREFIX="$shim:" daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "false" "chain: a delivery in flight refuses the resume"
assert_eq "$(jqf "$out" .reason)" "continuation-attempting" "chain: the refusal names the monitor's gap"
assert_contains "$(jqf "$out" .display)" "req-9" "chain: the refusal names the handoff"
[ ! -f "$FAKE_TMUX_STATE/new_window_args.log" ] || fail_test "chain: a window was opened"

out=$(HANDOFF_MODE=superseded HANDOFF_PATH_PREFIX="$shim:" daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "true" "chain: the resume proceeds once the chain is retired"
assert_eq "$(jqf "$out" '.superseded_continuations | join(",")')" "req-7" \
  "chain: the result names the superseded handoff"
assert_contains "$(comments "$repo" "$id")" "Superseded context handoffs: req-7." \
  "chain: the dispatch comment names it too"
assert_eq "$(grep -c . "$shim/calls")" "2" "chain: each resume asked exactly once"

# A fresh dispatch replaces no session, so it retires no chain.
fresh_id=$(new_story "$repo" "A story nobody dispatched before")
rm -f "$shim/calls"
out=$(HANDOFF_MODE=superseded HANDOFF_PATH_PREFIX="$shim:" daemon_dispatch "$repo" dispatch "$fresh_id")
assert_eq "$(jqf "$out" .ok)" "true" "fresh: the dispatch succeeds"
[ ! -f "$shim/calls" ] || fail_test "fresh: a fresh dispatch retired a handoff chain"
assert_eq "$(jqf "$out" '.superseded_continuations // "absent"')" "absent" "fresh: no chain is reported"

# The real verb, with no handoffs on record: the resume proceeds and reports none.
repo=$(mk_story_repo RHR)
id=$(new_story "$repo" "Resumed with no handoff on record")
out=$(daemon_dispatch "$repo" dispatch "$id")
lose_window "$(jqf "$out" .pane)"
out=$(daemon_dispatch "$repo" dispatch "$id" --resume --if-absent)
assert_eq "$(jqf "$out" .ok)" "true" "real verb: the resume proceeds"
assert_eq "$(jqf "$out" '.superseded_continuations // "absent"')" "absent" "real verb: nothing was superseded"

finish
