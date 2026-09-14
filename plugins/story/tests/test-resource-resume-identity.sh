#!/usr/bin/env bash
# SH-709: a cached server-local pane ID cannot authorize killing its replacement.
source "$(dirname "$0")/lib.sh"
repo=$(mk_story_repo RRI)
id=$(new_story "$repo" "Resume changed identity")
mk_dispatched "$repo" "$id" >/dev/null
(cd "$repo" && story move "$id" in-progress >/dev/null)
export FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$FAKE_TMUX_STATE")
export FAKE_TMUX_PANES="$id\t1\t%9"
export RESOURCE_REAL_GIT="$(type -P git)"
race_bin="$(mktemp -d /tmp/story-test-resume-race.XXXXXX)"
_TMP_REPOS+=("$race_bin")
cat >"$race_bin/git" <<'RACE_GIT'
#!/bin/sh
if [ "${3:-}" = rev-parse ] && [ "${4:-}" = --verify ] && [ "${5:-}" = 'HEAD^{commit}' ]; then
  printf 999999 >"$FAKE_TMUX_STATE/pane_pid"
  touch "$FAKE_TMUX_STATE/replacement_triggered"
fi
exec "$RESOURCE_REAL_GIT" "$@"
RACE_GIT
chmod +x "$race_bin/git"
worktree="$repo/.claude/worktrees/$id"
mkdir -p "$worktree/.claude"
printf 'original witness' >"$worktree/.claude/dispatch-sentinel.json"
out=$(cd "$repo" && PATH="$race_bin:$TESTS_DIR/fakes:$PATH" TMUX=fake,0,0 TMUX_PANE=%0 \
  STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 STORY_CONFIRM_DELAY=0 \
  STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
  bash "$SCRIPT" dispatch "$id" --resume)
[ -f "$FAKE_TMUX_STATE/replacement_triggered" ] || fail_test "replacement was not injected after inventory"
assert_eq "$(jqf "$out" .reason)" resource-identity-changed "resume refuses a replaced process"
assert_eq "$(cat "$worktree/.claude/dispatch-sentinel.json" 2>/dev/null)" 'original witness' "resume preserves the previous witness"
[ ! -f "$FAKE_TMUX_STATE/respawn_pane_args.log" ] || fail_test "replacement was respawned"
[ -d "$worktree" ] || fail_test "worktree was removed"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" in-progress "claim remains"
finish
