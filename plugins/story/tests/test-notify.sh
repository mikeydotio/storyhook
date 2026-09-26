#!/usr/bin/env bash
# SH-521: verifier remediation reaches only the exact provider-tagged pane and
# a multi-line diagnosis is submitted as one bracketed-paste prompt.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"
repo=$(mk_story_repo CDX)
id=$(new_story "$repo" "Verifier remediation")

out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" STORY_AGENT=codex \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      bash "$SCRIPT" dispatch "$id" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "true" "dispatch: Codex lane exists"
assert_eq "$(cat "$FAKE_TMUX_STATE/storyhook_agent")" "codex" \
  "dispatch: provider identity is stored on the window"

export FAKE_TMUX_PANES="$id	1	%1"
message="CENTRAL VERIFICATION RED
Fix the existing PR, then move $id back to verifying."
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 \
      bash "$SCRIPT" notify "$id" "$message" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "true" "notify: remediation delivered"
assert_eq "$(cat "$FAKE_TMUX_STATE/submitted")" "$message" \
  "notify: multi-line remediation submitted as one prompt"
assert_eq "$(tail -n 1 "$FAKE_TMUX_STATE/submit_keys.log")" "Tab" \
  "notify: Codex remediation uses its configured submit key"

saved_identity=$(cat "$FAKE_TMUX_STATE/pane_identity")
rm -f "$FAKE_TMUX_STATE/storyhook_agent" "$FAKE_TMUX_STATE/pane_identity"
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 \
      bash "$SCRIPT" notify "$id" "must not land" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "notify: untagged pane refused"
assert_eq "$(jqf "$out" .reason)" "pane-provider-unknown" \
  "notify: refusal identifies missing provider metadata"

printf 'codex' > "$FAKE_TMUX_STATE/storyhook_agent"
printf '%s' "$saved_identity" > "$FAKE_TMUX_STATE/pane_identity"
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_PANE_COMMAND=zsh bash "$SCRIPT" notify "$id" "must not land" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "notify: changed pane refused"
assert_eq "$(jqf "$out" .reason)" "pane-changed" \
  "notify: refusal identifies the unrelated occupant"

id=$(new_story "$repo" "Claude verifier remediation")
out=$(cd "$repo" && PATH="$FAKE_TMUX_DIR:$PATH" TMUX=fake TMUX_PANE=%0 \
  STORY_AGENT=claude STORY_READY_DELAY=0 STORY_CONFIRM_DELAY=0 \
  STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker bash "$SCRIPT" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" true "notify: Claude has its own managed registration"
export FAKE_TMUX_PANES="$id	1	%1"
remediate() {
  (cd "$repo" && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 STORY_CONFIRM_DELAY=0 \
    FAKE_TMUX_PANE_COMMAND=claude FAKE_TMUX_CAPTURE=marker "$@" bash "$SCRIPT" notify "$id" "$message" 2>&1)
}
pastes() { wc -l < "$FAKE_TMUX_STATE/pastes.log" 2>/dev/null | tr -d ' ' || printf 0; }
submits() { wc -l < "$FAKE_TMUX_STATE/submit_keys.log" 2>/dev/null | tr -d ' ' || printf 0; }
out=$(remediate env)
assert_eq "$(jqf "$out" .ok)" "true" "notify: Claude remediation delivered"
assert_eq "$(tail -n 1 "$FAKE_TMUX_STATE/submit_keys.log")" "Enter" \
  "notify: Claude remediation retains its configured submit key"

# SH-780: the remediation types only into an idle composer and submits only
# what it sees there. Before, it pasted and pressed Enter blind, and Enter on a
# dialog's selector row ('❯ 1. Yes') approves the dialog for the person.
printf '%s' "1. Yes" > "$FAKE_TMUX_STATE/input"
pastes_before=$(pastes); submits_before=$(submits)
out=$(remediate env)
assert_eq "$(jqf "$out" .ok)" false "busy composer: refused"
assert_eq "$(jqf "$out" .reason)" composer-busy "busy composer: named"
assert_contains "$(jqf "$out" .display)" "window" "busy composer: the refusal names the window"
assert_eq "$(pastes)" "$pastes_before" "busy composer: nothing pasted"
assert_eq "$(submits)" "$submits_before" "busy composer: no submit key"
: > "$FAKE_TMUX_STATE/input"

# No composer row at all is no evidence of an idle one.
pastes_before=$(pastes); submits_before=$(submits)
out=$(remediate env FAKE_TMUX_CAPTURE=legacy)
assert_eq "$(jqf "$out" .reason)" composer-busy "no composer row: refused as not idle"
assert_eq "$(pastes)" "$pastes_before" "no composer row: nothing pasted"
assert_eq "$(submits)" "$submits_before" "no composer row: no submit key"

# A paste that never shows in the composer is never submitted.
submits_before=$(submits)
out=$(remediate env FAKE_TMUX_DROP_PASTE=1)
assert_eq "$(jqf "$out" .ok)" false "dropped paste: refused"
assert_eq "$(jqf "$out" .reason)" delivery-failed "dropped paste: named"
assert_eq "$(submits)" "$submits_before" "dropped paste: no submit key was sent"
: > "$FAKE_TMUX_STATE/input"

# SH-650: a pane whose process has exited under remain-on-exit is a corpse, not
# an occupant. tmux freezes #{pane_current_command} at its last live value, so
# pane_runs still answers yes; only #{pane_dead} tells the two apart. Before
# this refusal existed the paste itself failed ("target pane has exited") and
# the verifier read the dead pane as `delivery-failed` -- the one refusal that
# means the agent IS live. Kill the fake's placeholder process and prove the
# refusal names death, before any paste is attempted.
pastes_before=$(wc -l < "$FAKE_TMUX_STATE/pastes.log" 2>/dev/null || printf 0)
pane_pid=$(cat "$FAKE_TMUX_STATE/pane_pid")
kill -9 "$pane_pid" 2>/dev/null || true
while kill -0 "$pane_pid" 2>/dev/null; do sleep 0.1; done
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_PANE_COMMAND=claude bash "$SCRIPT" notify "$id" "must not land" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "notify: dead pane refused"
assert_eq "$(jqf "$out" .reason)" "pane-dead" \
  "notify: refusal names the exited pane, not a delivery failure"
assert_contains "$(jqf "$out" .display)" "exited" \
  "notify: dead-pane refusal says the process is gone"
assert_eq "$(wc -l < "$FAKE_TMUX_STATE/pastes.log" 2>/dev/null || printf 0)" "$pastes_before" \
  "notify: nothing is pasted into a dead pane"

finish
