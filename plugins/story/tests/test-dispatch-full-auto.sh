#!/usr/bin/env bash
# SH-461: the engine's autonomous lane has a distinct marker and launch
# override boundary. It reuses SH-511's provider posture and approval gates;
# this file pins only dispatch identity, containment, and override isolation.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

unset STORY_LAUNCH_CMD STORY_FULL_AUTO_LAUNCH_CMD
unset STORYHOOK_AUTO STORYHOOK_FULL_AUTO

repo=$(mk_story_repo)
id=$(new_story "$repo" "Full Auto launch boundary")

dry() {
  (cd "$repo" && STORY_DRY_RUN=1 STORY_COUNCIL=off \
    bash "$SCRIPT" dispatch "$id" "$@" 2>&1)
}

# --full-auto is a strict engine-only modifier: it requires both --auto and a
# caller-selected story, and duplicates are rejected before story state, tmux,
# or git resources can change.
out=$(cd "$repo" && bash "$SCRIPT" dispatch "$id" --full-auto 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "full-auto without auto: refused"
assert_contains "$(jqf "$out" .display)" "requires --auto" \
  "full-auto without auto: names the missing contract"

out=$(cd "$repo" && bash "$SCRIPT" dispatch --next --auto --full-auto 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "full-auto with next: refused"
assert_contains "$(jqf "$out" .display)" "requires a named story id" \
  "full-auto with next: names the engine's selected-story contract"

out=$(cd "$repo" && bash "$SCRIPT" dispatch "$id" --auto --full-auto --full-auto 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "duplicate full-auto: refused"
assert_contains "$(jqf "$out" .display)" "only once" \
  "duplicate full-auto: names duplication"

state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "todo" "invalid Full Auto compositions: story state untouched"
[ ! -d "$repo/.claude/worktrees" ] \
  || fail_test "invalid Full Auto composition created a worktree container"

# The per-window marker matrix is total and unambiguous. Empty values actively
# contain a tmux session environment inherited from an earlier lane.
attended=$(dry)
ordinary_auto=$(dry --auto)
full_auto=$(dry --auto --full-auto)

attended_commands=$(jqf "$attended" '.commands|join(" ")')
ordinary_commands=$(jqf "$ordinary_auto" '.commands|join(" ")')
full_commands=$(jqf "$full_auto" '.commands|join(" ")')

assert_contains "$attended_commands" \
  "-e STORYHOOK_AUTO= -e STORYHOOK_FULL_AUTO=" \
  "attended marker row: both markers explicitly empty"
assert_contains "$ordinary_commands" \
  "-e STORYHOOK_AUTO=$id -e STORYHOOK_FULL_AUTO=" \
  "ordinary Auto marker row: only STORYHOOK_AUTO selected"
assert_contains "$full_commands" \
  "-e STORYHOOK_AUTO= -e STORYHOOK_FULL_AUTO=$id" \
  "Full Auto marker row: only STORYHOOK_FULL_AUTO selected"
case "$attended_commands $ordinary_commands $full_commands" in
  *set-environment*) fail_test "a lane marker was written to tmux session-global environment" ;;
esac

# Non-Full-Auto JSON remains compatible. Full Auto alone identifies itself,
# while reusing the exact provider default SH-511 already proved live.
assert_eq "$(jqf "$attended" 'has("full_auto")')" "false" \
  "attended JSON: no Full Auto metadata"
assert_eq "$(jqf "$ordinary_auto" 'has("full_auto")')" "false" \
  "ordinary Auto JSON: no Full Auto metadata"
assert_eq "$(jqf "$full_auto" .full_auto)" "true" \
  "Full Auto JSON: mode reported"
assert_eq "$(jqf "$full_auto" .launch_source)" "builtin" \
  "Full Auto builtin: source reported"
assert_eq "$(jqf "$full_auto" .launch_overridden)" "false" \
  "Full Auto builtin: not reported as overridden"
assert_contains "$full_commands" \
  "claude --plugin-dir '$PLUGIN_ROOT' --permission-mode plan --model opusplan --settings '{\"permissions\":{\"defaultMode\":\"acceptEdits\"}}'" \
  "Full Auto builtin: reuses SH-511's Claude Auto provider command"
assert_contains "$(jqf "$full_auto" .display)" "Full Auto (--auto --full-auto)" \
  "Full Auto display: names the distinct engine mode"
assert_contains "$(jqf "$full_auto" .display)" \
  "Full Auto launch source: builtin (launch_overridden=false)" \
  "Full Auto display: reports builtin launch metadata"

# The daemon's general expert override must never weaken an engine lane. It is
# ignored and reported by name, while ordinary Auto still preserves it.
ordinary_general=$(cd "$repo" && STORY_DRY_RUN=1 STORY_COUNCIL=off \
  STORY_LAUNCH_CMD="claude --permission-mode general-override" \
  bash "$SCRIPT" dispatch "$id" --auto 2>&1)
assert_contains "$(jqf "$ordinary_general" '.commands|join(" ")')" \
  "claude --permission-mode general-override" \
  "ordinary Auto: general override behavior preserved"
assert_eq "$(jqf "$ordinary_general" .launch_source)" "STORY_LAUNCH_CMD" \
  "ordinary Auto: general override source preserved"

ignored_general=$(cd "$repo" && STORY_DRY_RUN=1 STORY_COUNCIL=off \
  STORY_LAUNCH_CMD="claude --permission-mode general-override" \
  bash "$SCRIPT" dispatch "$id" --auto --full-auto 2>&1)
ignored_commands=$(jqf "$ignored_general" '.commands|join(" ")')
assert_contains "$ignored_commands" \
  "claude --plugin-dir '$PLUGIN_ROOT' --permission-mode plan --model opusplan --settings '{\"permissions\":{\"defaultMode\":\"acceptEdits\"}}'" \
  "Full Auto: general override cannot replace the builtin"
case "$ignored_commands" in
  *general-override*) fail_test "Full Auto obeyed STORY_LAUNCH_CMD" ;;
esac
assert_eq "$(jqf "$ignored_general" .launch_source)" "builtin" \
  "Full Auto ignored general override: source remains builtin"
assert_eq "$(jqf "$ignored_general" .launch_overridden)" "false" \
  "Full Auto ignored general override: builtin is not an override"
assert_eq "$(jqf "$ignored_general" .ignored_general_override)" "STORY_LAUNCH_CMD" \
  "Full Auto ignored general override: JSON reports the ignored seam"
assert_contains "$(jqf "$ignored_general" .display)" "Ignored STORY_LAUNCH_CMD" \
  "Full Auto ignored general override: display reports containment"

dedicated=$(cd "$repo" && STORY_DRY_RUN=1 STORY_COUNCIL=off \
  STORY_LAUNCH_CMD="claude general" \
  STORY_FULL_AUTO_LAUNCH_CMD="claude dedicated" \
  bash "$SCRIPT" dispatch "$id" --auto --full-auto 2>&1)
assert_contains "$(jqf "$dedicated" '.commands|join(" ")')" "claude dedicated" \
  "Full Auto dedicated override: selected"
assert_eq "$(jqf "$dedicated" .launch_source)" "STORY_FULL_AUTO_LAUNCH_CMD" \
  "Full Auto dedicated override: source reported"
assert_eq "$(jqf "$dedicated" .launch_overridden)" "true" \
  "Full Auto dedicated override: override reported"
assert_eq "$(jqf "$dedicated" .ignored_general_override)" "STORY_LAUNCH_CMD" \
  "Full Auto dedicated override: inherited general seam still reported ignored"
assert_contains "$(jqf "$dedicated" .display)" \
  "Launch override STORY_FULL_AUTO_LAUNCH_CMD is active" \
  "Full Auto dedicated override: visible in display"
assert_contains "$(jqf "$dedicated" .display)" \
  "Full Auto launch source: STORY_FULL_AUTO_LAUNCH_CMD (launch_overridden=true)" \
  "Full Auto dedicated override: display reports launch metadata"

claude_commands=$(jqf "$full_auto" '.commands|join(" ")')
assert_contains "$claude_commands" \
  "env STORYHOOK_AUTO= STORYHOOK_FULL_AUTO=$id" \
  "Claude Full Auto: watcher receives the selected marker"
assert_contains "$claude_commands" "--approve-claude-plan <pane> <pane-pid>" \
  "Claude Full Auto: exact-pane watcher is armed"

# Codex reuses SH-511's provider command and exact-pane approval watcher. The
# selected Full Auto marker is carried into that watcher; its predicate and
# timing remain owned by the landed hook.
codex=$(dry --agent=codex --auto --full-auto)
codex_commands=$(jqf "$codex" '.commands|join(" ")')
assert_contains "$codex_commands" \
  "codex --no-alt-screen -c check_for_update_on_startup=false --approve-for-me --dangerously-bypass-hook-trust" \
  "Codex Full Auto: reuses SH-511's provider command"
assert_contains "$codex_commands" \
  "env STORYHOOK_AUTO= STORYHOOK_FULL_AUTO=$id" \
  "Codex Full Auto: watcher receives the selected marker"
assert_contains "$codex_commands" "--approve-codex-plan <pane>" \
  "Codex Full Auto: exact-pane watcher remains armed"

# The fake-tmux path pins the same matrix at the actual new-window argv
# boundary, not only in dry-run prose.
real_marker_row() {
  local label="$1"
  shift
  local real_repo real_id real_out args
  real_repo=$(mk_story_repo)
  real_id=$(new_story "$real_repo" "$label marker row")
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-full-auto-tmux.XXXXXX)
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  real_out=$(
    cd "$real_repo" &&
      PATH="$FAKE_TMUX_DIR:$PATH" TMUX="fake,0,0" TMUX_PANE=%0 \
      STORY_COUNCIL=off STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$real_id" "$@" 2>&1
  )
  assert_eq "$(jqf "$real_out" .ok)" "true" "$label real dispatch: succeeds"
  args=$(cat "$FAKE_TMUX_STATE/new_window_args.log")
  case "$label" in
    attended)
      assert_contains "$args" "-e STORYHOOK_AUTO= -e STORYHOOK_FULL_AUTO=" \
        "attended real marker row" ;;
    ordinary)
      assert_contains "$args" "-e STORYHOOK_AUTO=$real_id -e STORYHOOK_FULL_AUTO=" \
        "ordinary Auto real marker row" ;;
    full)
      assert_contains "$args" "-e STORYHOOK_AUTO= -e STORYHOOK_FULL_AUTO=$real_id" \
        "Full Auto real marker row" ;;
  esac
}

real_marker_row attended
real_marker_row ordinary --auto
real_marker_row full --auto --full-auto

finish
