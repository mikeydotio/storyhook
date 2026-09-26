#!/usr/bin/env bash
# SH-772 decision D5: `notify <id> <prompt> --registered-session` resumes the
# story's registered session when the block's interrupt was never
# acknowledged. It never adopts, types nothing unless the composer is idle
# (a dialog's '❯ 1. Yes' row reads as text, and Enter would approve it), sends
# the submit key only after the prompt is seen, and names the session it bound.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"
repo=$(mk_story_repo RGS)
id=$(new_story "$repo" "Resume after an unacknowledged interrupt")

out=$(cd "$repo" && PATH="$FAKE_TMUX_DIR:$PATH" TMUX=fake TMUX_PANE=%0 \
  STORY_AGENT=claude STORY_READY_DELAY=0 STORY_CONFIRM_DELAY=0 \
  STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker FAKE_TMUX_PANE_CHILD=1 \
  bash "$SCRIPT" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" true "dispatch: the session is registered"
export FAKE_TMUX_PANES="$id	1	%1"
prompt="Your story experienced a temporary block, which has been lifted."

resume() {
  (cd "$repo" && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 \
    STORY_CONFIRM_DELAY=0 FAKE_TMUX_CAPTURE=marker FAKE_TMUX_PANE_COMMAND=claude \
    "$@" bash "$SCRIPT" notify "$id" "$prompt" --registered-session 2>&1)
}
pastes() { wc -l < "$FAKE_TMUX_STATE/pastes.log" 2>/dev/null | tr -d ' ' || printf 0; }
submits() { wc -l < "$FAKE_TMUX_STATE/submit_keys.log" 2>/dev/null | tr -d ' ' || printf 0; }

# A composer holding anything — a draft, or a dialog's selector row — is not idle.
printf '%s' "1. Yes" > "$FAKE_TMUX_STATE/input"
pastes_before=$(pastes); submits_before=$(submits)
out=$(resume env)
assert_eq "$(jqf "$out" .ok)" false "busy composer: refused"
assert_eq "$(jqf "$out" .reason)" composer-busy "busy composer: named"
assert_eq "$(pastes)" "$pastes_before" "busy composer: nothing pasted"
assert_eq "$(submits)" "$submits_before" "busy composer: no submit key"
: > "$FAKE_TMUX_STATE/input"

# A paste that never lands is never submitted.
pastes_before=$(pastes); submits_before=$(submits)
out=$(resume env FAKE_TMUX_DROP_PASTE=1)
assert_eq "$(jqf "$out" .ok)" false "dropped paste: refused"
assert_eq "$(jqf "$out" .reason)" delivery-failed "dropped paste: named"
assert_eq "$(submits)" "$submits_before" "dropped paste: no submit key was sent"
: > "$FAKE_TMUX_STATE/input"

# The registered session at an idle composer receives the prompt, submitted once.
out=$(resume env)
assert_eq "$(jqf "$out" .ok)" true "idle composer: resumed"
assert_eq "$(cat "$FAKE_TMUX_STATE/submitted")" "$prompt" "idle composer: the exact prompt"
assert_eq "$(tail -n 1 "$FAKE_TMUX_STATE/submit_keys.log")" Enter "idle composer: Claude's submit key"
target=$(jqf "$out" .target)
if [ -z "$target" ] || [ "$target" = null ]; then
  fail_test "idle composer: no bound session named in $out"
fi

# An unregistered pane is never adopted by a delayed resume.
saved_identity=$(cat "$FAKE_TMUX_STATE/pane_identity")
rm -f "$FAKE_TMUX_STATE/pane_identity"
pastes_before=$(pastes)
out=$(resume env)
assert_eq "$(jqf "$out" .ok)" false "unregistered pane: refused"
assert_eq "$(jqf "$out" .reason)" pane-provider-unknown "unregistered pane: named"
assert_eq "$(pastes)" "$pastes_before" "unregistered pane: nothing pasted"
if [ -f "$FAKE_TMUX_STATE/pane_identity" ]; then
  fail_test "unregistered pane: the resume adopted the pane"
fi
printf '%s' "$saved_identity" > "$FAKE_TMUX_STATE/pane_identity"

finish
