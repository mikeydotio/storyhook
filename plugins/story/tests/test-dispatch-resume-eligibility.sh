#!/usr/bin/env bash
# SH-850: a resume that reuses a story's active claim skips the ready gate, so
# it used to relaunch an agent on a story that an awaiting hold or an open
# blocker held -- the very agent a block exists to stop (SH-690). A resume now
# asks `story session-eligibility` first and refuses, changing nothing, unless
# the tracker says the claimed session may continue. A verb that cannot answer
# is refused by its own name, never read as a pass.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# The daemon's envelope, as test-notify-redispatch.sh documents it.
daemon_dispatch() {
  local repo="$1"
  shift
  (
    cd "$repo" \
      && PATH="${ELIGIBILITY_PATH_PREFIX:-}$FAKE_TMUX_DIR:$PATH" \
        STORY_TARGET_SESSION="$(slug_for "$repo")" STORY_CREATE_SESSION=1 \
        STORY_COUNCIL=off STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" "$@" 2>&1
  )
}

# Dispatches <id>, then removes its window: the lost agent a resume replaces.
dispatch_then_lose_window() {
  local repo="$1" id="$2" out window_id
  out=$(daemon_dispatch "$repo" dispatch "$id")
  assert_eq "$(jqf "$out" .ok)" "true" "$id: the original dispatch succeeds"
  window_id=$("$FAKE_TMUX_DIR/tmux" display-message -p -t "$(jqf "$out" .pane)" '#{window_id}')
  "$FAKE_TMUX_DIR/tmux" kill-window -t "$window_id"
  unset FAKE_TMUX_PANES
  rm -f "$FAKE_TMUX_STATE/new_window_args.log" "$FAKE_TMUX_STATE/respawn_pane_args.log"
}

# A block change on an in-progress story makes the daemon's block-delivery
# worker try an interrupt or a resume under the story's workspace lock. Wait
# for that attempt's receipt comment, so the resume below never meets a
# workspace the worker still holds.
await_delivery_receipt() {
  local repo="$1" id="$2" action="$3" tries=0
  until (cd "$repo" && story show "$id" --json) | jq -e --arg a " $action " \
      '[.story.story.comments[].text | select(startswith("AGENT BLOCK DELIVERY") and contains($a))] | length > 0' \
      >/dev/null 2>&1; do
    tries=$((tries + 1))
    [ "$tries" -le 600 ] || { fail_test "$id: no $action delivery receipt"; return; }
    sleep 0.1
  done
}

# Asserts a refused resume launched nothing and left the story as it was.
assert_untouched() {
  local repo="$1" id="$2" label="$3"
  [ ! -f "$FAKE_TMUX_STATE/new_window_args.log" ] || fail_test "$label: a window was opened"
  [ ! -f "$FAKE_TMUX_STATE/respawn_pane_args.log" ] || fail_test "$label: a pane was respawned"
  assert_eq "$(cd "$repo" && story show "$id" --json | jq -r .story.story.state)" "in-progress" \
    "$label: the story keeps its claim"
}

# --- an awaiting hold refuses the resume --------------------------------------
repo=$(mk_story_repo RVA)
id=$(new_story "$repo" "Held by an operator")
dispatch_then_lose_window "$repo" "$id"
(cd "$repo" && story block "$id" "operator is inspecting the lane" >/dev/null)
await_delivery_receipt "$repo" "$id" interrupt
out=$(daemon_dispatch "$repo" dispatch "$id" --resume)
assert_eq "$(jqf "$out" .ok)" "false" "awaiting: the resume is refused"
assert_eq "$(jqf "$out" .reason)" "resume-ineligible" "awaiting: the refusal names eligibility"
assert_contains "$(jqf "$out" .display)" "(awaiting)" "awaiting: the tracker's reason is named"
assert_contains "$(jqf "$out" .display)" "operator is inspecting the lane" "awaiting: the hold text is quoted"
assert_untouched "$repo" "$id" "awaiting"

# Control: the same story resumes once the hold is lifted.
(cd "$repo" && story unblock "$id" >/dev/null)
await_delivery_receipt "$repo" "$id" resume
out=$(daemon_dispatch "$repo" dispatch "$id" --resume)
assert_eq "$(jqf "$out" .ok)" "true" "eligible: the resume proceeds once the hold is lifted"
assert_eq "$(jqf "$out" .resumed)" "true" "eligible: it is a resume"

# --- an open blocker refuses the resume ---------------------------------------
repo=$(mk_story_repo RVB)
id=$(new_story "$repo" "Waits on another story")
blocker=$(new_story "$repo" "The blocker")
dispatch_then_lose_window "$repo" "$id"
(cd "$repo" && story relate "$id" blocked-by "$blocker" >/dev/null)
await_delivery_receipt "$repo" "$id" interrupt
out=$(daemon_dispatch "$repo" dispatch "$id" --resume)
assert_eq "$(jqf "$out" .ok)" "false" "blocked-by: the resume is refused"
assert_eq "$(jqf "$out" .reason)" "resume-ineligible" "blocked-by: the refusal names eligibility"
assert_contains "$(jqf "$out" .display)" "(blocked)" "blocked-by: the tracker's reason is named"
assert_contains "$(jqf "$out" .display)" "$blocker" "blocked-by: the blocker is named"
assert_untouched "$repo" "$id" "blocked-by"

# --- a verb that cannot answer is refused, never read as a pass ---------------
repo=$(mk_story_repo RVU)
id=$(new_story "$repo" "Eligibility cannot be asked")
dispatch_then_lose_window "$repo" "$id"
shim=$(mktemp -d /tmp/story-test-eligibility.XXXXXX)
_TMP_REPOS+=("$shim")
real_story=$(command -v story)
cat >"$shim/story" <<SHIM
#!/usr/bin/env bash
case " \$* " in
  *' session-eligibility '*) printf 'error: session eligibility: project has no unambiguous active state role\n' >&2; exit 2 ;;
esac
exec "$real_story" "\$@"
SHIM
chmod +x "$shim/story"
out=$(ELIGIBILITY_PATH_PREFIX="$shim:" daemon_dispatch "$repo" dispatch "$id" --resume)
assert_eq "$(jqf "$out" .ok)" "false" "unanswered: the resume is refused"
assert_eq "$(jqf "$out" .reason)" "resume-eligibility-unavailable" "unanswered: the refusal names the missing answer"
assert_contains "$(jqf "$out" .display)" "no unambiguous active state role" "unanswered: the verb's own words are kept"
assert_untouched "$repo" "$id" "unanswered"

finish
