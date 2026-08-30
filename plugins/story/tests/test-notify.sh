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

rm -f "$FAKE_TMUX_STATE/storyhook_agent"
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 \
      bash "$SCRIPT" notify "$id" "must not land" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "notify: untagged pane refused"
assert_eq "$(jqf "$out" .reason)" "pane-provider-unknown" \
  "notify: refusal identifies missing provider metadata"

printf 'codex' > "$FAKE_TMUX_STATE/storyhook_agent"
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_PANE_COMMAND=zsh bash "$SCRIPT" notify "$id" "must not land" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "notify: changed pane refused"
assert_eq "$(jqf "$out" .reason)" "pane-changed" \
  "notify: refusal identifies the unrelated occupant"

finish
