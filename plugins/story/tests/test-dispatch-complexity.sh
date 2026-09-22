#!/usr/bin/env bash
# Exercise production resolution through the real CLI and the dry-run launch.
source "$(dirname "$0")/lib.sh"
export PATH="$TESTS_DIR/fakes:$PATH"
export TMUX="fake,0,0" TMUX_PANE="%0"
repo=$(mk_story_repo CPX)
id=$(new_story "$repo" "Complexity policy")
for agent in codex claude; do
  case "$agent" in codex) model=gpt-6-astra ;; claude) model=fable ;; esac
  for pair in low:medium medium:high high:xhigh; do
    level=${pair%:*} effort=${pair#*:}
    (cd "$repo" && story set "$id" --complexity "$level") >/dev/null
    out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" --agent="$agent")
    assert_eq "$(jqf "$out" .ok)" true "$agent/$level resolves: $out"
    assert_eq "$(jqf "$out" .model)" "$model" "$agent/$level model"
    assert_eq "$(jqf "$out" .effort)" "$effort" "$agent/$level effort"
    assert_eq "$(jqf "$out" .model_source)" builtin "$agent/$level origin"
  done
done
(cd "$repo" && story dispatch-policy set --agent codex --complexity high --model gpt-5.6-sol) >/dev/null
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" --agent=codex --effort=low)
assert_eq "$(jqf "$out" .model)" gpt-5.6-sol "project model"
assert_eq "$(jqf "$out" .effort)" low "explicit effort"
assert_eq "$(jqf "$out" .model_source)" project "project origin"
assert_eq "$(jqf "$out" .effort_source)" flag "explicit origin"
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch --next --agent=codex)
assert_eq "$(jqf "$out" .model)" gpt-5.6-sol "next uses claimed story complexity"
assert_eq "$(jqf "$out" .effort)" xhigh "next effort"
# Engine child dispatch uses the executable child's policy.
for level in low high; do
  (cd "$repo" && story set "$id" --complexity "$level") >/dev/null
  out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" --agent=codex --auto --full-auto)
  expected=medium; [ "$level" != high ] || expected=xhigh
  assert_eq "$(jqf "$out" .effort)" "$expected" "engine $level child effort"
done

# A wholesale command remains authoritative even for an attended launch.
out=$(cd "$repo" && STORY_DRY_RUN=1 STORY_LAUNCH_CMD='claude --model sonnet' bash "$SCRIPT" dispatch "$id" --agent=claude)
assert_eq "$(jqf "$out" .model_source)" custom-command "attended custom launch bypasses policy"
assert_contains "$out" 'claude --model sonnet' "custom launch remains intact"
assert_eq "$(jqf "$out" 'has("model")')" false "custom launch makes no model claim"
finish
