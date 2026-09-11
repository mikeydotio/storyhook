#!/usr/bin/env bash
# SH-664: neither force nor resume can cross an owned reset workspace lock.
source "$(dirname "$0")/lib.sh"
repo=$(mk_story_repo)
id=$(new_story "$repo" "Reset exclusion")
(cd "$repo" && story move "$id" in-progress >/dev/null)
common=$(cd "$repo" && git rev-parse --path-format=absolute --git-common-dir)
mkdir -p "$common/storyhook/workspace-locks"
exec 8>>"$common/storyhook/workspace-locks/$id.lock"
python3 - <<'PY'
import fcntl
fcntl.flock(8, fcntl.LOCK_EX | fcntl.LOCK_NB)
PY
for mode in --force --resume; do
  out=$(cd "$repo" && PATH="$TESTS_DIR/fakes:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" bash "$SCRIPT" dispatch "$id" "$mode" 2>&1)
  assert_eq "$(jqf "$out" .ok)" "false" "$mode refuses an owned workspace"
  assert_contains "$(jqf "$out" .display)" "workspace is busy" "$mode diagnoses the exact exclusion"
done
[ ! -d "$repo/.claude/worktrees/$id" ] || fail_test "busy dispatch created a worktree"
# The verifier can reuse the same open description while retaining exclusion.
out=$(cd "$repo" && PATH="$TESTS_DIR/fakes:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" \
  STORY_WORKSPACE_LOCK_FD=8 STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
  STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
  bash "$SCRIPT" dispatch "$id" --force 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "inherited verifier ownership permits its own dispatch"
exec 8>&-
finish
