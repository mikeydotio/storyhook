#!/usr/bin/env bash
# SH-850 D5: every confirmed launch records the settings a person chose, so a
# Resume after a reboot -- which erases the pane's environment -- can offer
# them again. The record lives beside the cleanup marker in the worktree's
# private Git directory, holds only explicit selectors (an empty one means the
# default, which a resume re-resolves the same way), and is written only after
# the handoff is confirmed, so a failed attempt never overwrites it.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"
FAKE_BIN=$(mktemp -d /tmp/story-test-launch-bin.XXXXXX)
_TMP_REPOS+=("$FAKE_BIN")
printf '#!/bin/sh\nexit 0\n' >"$FAKE_BIN/codex"
chmod +x "$FAKE_BIN/codex"

dispatch() {
  local repo="$1"
  shift
  (
    cd "$repo" \
      && PATH="$FAKE_BIN:$FAKE_TMUX_DIR:$PATH" \
        STORY_TARGET_SESSION="$(slug_for "$repo")" STORY_CREATE_SESSION=1 \
        STORY_COUNCIL=off STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
        FAKE_TMUX_CODEX_SENTINEL_MODE=identity FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" \
        bash "$SCRIPT" "$@" 2>&1
  )
}

record_of() {
  local worktree="$1"
  cat "$(git -C "$worktree" rev-parse --absolute-git-dir)/storyhook-launch-v1.json"
}

repo=$(mk_story_repo RLR)
slug=$(slug_for "$repo")

# Attended Claude with explicit selectors.
id=$(new_story "$repo" "Attended with selectors")
out=$(dispatch "$repo" dispatch "$id" --model=opus --effort=high)
assert_eq "$(jqf "$out" .ok)" "true" "attended: the dispatch succeeds"
record=$(record_of "$(jqf "$out" .worktree_path)")
assert_eq "$(jqf "$record" .version)" "1" "attended: versioned record"
assert_eq "$(jqf "$record" .project_slug)" "$slug" "attended: names the project"
assert_eq "$(jqf "$record" .story_id)" "$id" "attended: names the story"
assert_eq "$(jqf "$record" .provider)" "claude" "attended: the provider"
assert_eq "$(jqf "$record" .model)" "opus" "attended: the chosen model"
assert_eq "$(jqf "$record" .effort)" "high" "attended: the chosen effort"
assert_eq "$(jqf "$record" .speed)" "null" "attended: no speed was chosen"
assert_eq "$(jqf "$record" .autonomy)" "attended" "attended: the autonomy"
[ "$(jqf "$record" .recorded_at)" != null ] || fail_test "attended: the record is timestamped"

# Autonomous Claude, defaults except a fast speed.
id=$(new_story "$repo" "Autonomous and fast")
out=$(dispatch "$repo" dispatch "$id" --auto --speed=fast)
assert_eq "$(jqf "$out" .ok)" "true" "auto: the dispatch succeeds"
record=$(record_of "$(jqf "$out" .worktree_path)")
assert_eq "$(jqf "$record" .autonomy)" "auto" "auto: the autonomy"
assert_eq "$(jqf "$record" .model)" "null" "auto: an unchosen model stays the default"
assert_eq "$(jqf "$record" .effort)" "null" "auto: an unchosen effort stays the default"
assert_eq "$(jqf "$record" .speed)" "fast" "auto: the chosen speed"

# A Full Auto lane records itself as such.
id=$(new_story "$repo" "A Full Auto lane")
out=$(dispatch "$repo" dispatch "$id" --auto --full-auto)
assert_eq "$(jqf "$out" .ok)" "true" "full-auto: the dispatch succeeds"
assert_eq "$(jqf "$(record_of "$(jqf "$out" .worktree_path)")" .autonomy)" "full-auto" \
  "full-auto: the autonomy"

# Codex records its own provider.
id=$(new_story "$repo" "Attended Codex")
out=$(STORY_AGENT=codex dispatch "$repo" dispatch "$id" --agent=codex)
assert_eq "$(jqf "$out" .ok)" "true" "codex: the dispatch succeeds"
assert_eq "$(jqf "$(record_of "$(jqf "$out" .worktree_path)")" .provider)" "codex" \
  "codex: the provider"

# A failed resume attempt leaves the last confirmed launch's record in place.
id=$(new_story "$repo" "A resume that never got ready")
out=$(dispatch "$repo" dispatch "$id" --model=opus)
worktree=$(jqf "$out" .worktree_path)
before=$(record_of "$worktree")
window_id=$("$FAKE_TMUX_DIR/tmux" display-message -p -t "$(jqf "$out" .pane)" '#{window_id}')
"$FAKE_TMUX_DIR/tmux" kill-window -t "$window_id"
unset FAKE_TMUX_PANES
out=$(FAKE_TMUX_SUPPRESS_SENTINEL=1 STORY_READY_ATTEMPTS=3 \
  dispatch "$repo" dispatch "$id" --resume --if-absent --model=sonnet)
assert_eq "$(jqf "$out" .ok)" "false" "failed resume: the attempt is not ready"
assert_eq "$(record_of "$worktree")" "$before" "failed resume: the confirmed record is untouched"

finish
