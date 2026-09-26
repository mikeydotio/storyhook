#!/usr/bin/env bash
# The two session-bound resume forms of `notify`:
#   <id> <prompt> --registered-session         resume the story's registered
#       session when the block's interrupt was never acknowledged (SH-772 D5);
#   <id> <prompt> --expected-target <target>   resume the session a Delivered
#       interrupt acknowledged (SH-690), which SH-780 put under the same guard.
# Neither adopts. Each types nothing unless the composer is idle (a dialog's
# '❯ 1. Yes' row reads as text, and Enter would approve it), sends the submit key
# only after the prompt is seen, and names the session it bound.
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

# resume <registered|expected> <command prefix...> — one resume in that form.
target=""
resume() {
  local flag=(--registered-session)
  [ "$1" = registered ] || flag=(--expected-target "$target")
  shift
  (cd "$repo" && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 \
    STORY_CONFIRM_DELAY=0 FAKE_TMUX_CAPTURE=marker FAKE_TMUX_PANE_COMMAND=claude \
    "$@" bash "$SCRIPT" notify "$id" "$prompt" "${flag[@]}" 2>&1)
}
pastes() { wc -l < "$FAKE_TMUX_STATE/pastes.log" 2>/dev/null | tr -d ' ' || printf 0; }
submits() { wc -l < "$FAKE_TMUX_STATE/submit_keys.log" 2>/dev/null | tr -d ' ' || printf 0; }

# The registered session at an idle composer receives the prompt, submitted
# once, and the answer names the session it bound: that is the target a later
# --expected-target resume must match.
out=$(resume registered env)
assert_eq "$(jqf "$out" .ok)" true "registered, idle composer: resumed"
target=$(jqf "$out" .target)
if [ -z "$target" ] || [ "$target" = null ]; then
  fail_test "registered, idle composer: no bound session named in $out"
fi

for form in registered expected; do
  # A composer holding anything -- a draft, or a dialog's selector row -- is not idle.
  printf '%s' "1. Yes" > "$FAKE_TMUX_STATE/input"
  pastes_before=$(pastes); submits_before=$(submits)
  out=$(resume "$form" env)
  assert_eq "$(jqf "$out" .ok)" false "$form, busy composer: refused"
  assert_eq "$(jqf "$out" .reason)" composer-busy "$form, busy composer: named"
  assert_eq "$(pastes)" "$pastes_before" "$form, busy composer: nothing pasted"
  assert_eq "$(submits)" "$submits_before" "$form, busy composer: no submit key"
  : > "$FAKE_TMUX_STATE/input"

  # A paste that never lands is never submitted.
  pastes_before=$(pastes); submits_before=$(submits)
  out=$(resume "$form" env FAKE_TMUX_DROP_PASTE=1)
  assert_eq "$(jqf "$out" .ok)" false "$form, dropped paste: refused"
  assert_eq "$(jqf "$out" .reason)" delivery-failed "$form, dropped paste: named"
  assert_eq "$(submits)" "$submits_before" "$form, dropped paste: no submit key was sent"
  : > "$FAKE_TMUX_STATE/input"

  # An idle composer receives the exact prompt, with Claude's submit key.
  out=$(resume "$form" env)
  assert_eq "$(jqf "$out" .ok)" true "$form, idle composer: resumed"
  assert_eq "$(cat "$FAKE_TMUX_STATE/submitted")" "$prompt" "$form, idle composer: the exact prompt"
  assert_eq "$(tail -n 1 "$FAKE_TMUX_STATE/submit_keys.log")" Enter "$form, idle composer: Claude's submit key"
done

# An unregistered pane is never adopted by a delayed resume.
saved_identity=$(cat "$FAKE_TMUX_STATE/pane_identity")
rm -f "$FAKE_TMUX_STATE/pane_identity"
pastes_before=$(pastes)
out=$(resume registered env)
assert_eq "$(jqf "$out" .ok)" false "unregistered pane: refused"
assert_eq "$(jqf "$out" .reason)" pane-provider-unknown "unregistered pane: named"
assert_eq "$(pastes)" "$pastes_before" "unregistered pane: nothing pasted"
if [ -f "$FAKE_TMUX_STATE/pane_identity" ]; then
  fail_test "unregistered pane: the resume adopted the pane"
fi
printf '%s' "$saved_identity" > "$FAKE_TMUX_STATE/pane_identity"

finish
