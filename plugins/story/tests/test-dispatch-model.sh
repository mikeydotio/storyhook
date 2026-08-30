#!/usr/bin/env bash
# `story.sh dispatch <id> --model= --effort= --speed=` flag parsing (SH-517):
# single-occurrence, validated against the resolved agent's own catalog
# BEFORE any side effect (no claim, no worktree, no window), and STORY_MODEL/
# STORY_EFFORT/STORY_SPEED as the env-seam fallback beneath an explicit flag --
# the same precedence STORY_AGENT already has beneath --agent.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

dispatch_real() {
  local dir="$1" id="$2"; shift 2
  (
    cd "$dir" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch "$id" "$@" 2>&1
  )
}

# A valid --model/--effort/--speed selection dispatches normally and the
# helper's own success report echoes the resolved model, matching how it
# already echoes `agent`.
repo=$(mk_story_repo MDL)
id=$(new_story "$repo" "Model selector happy path")
out=$(dispatch_real "$repo" "$id" --model=haiku --effort=max --speed=fast)
assert_eq "$(jqf "$out" .ok)" "true" "valid selection: ok"
assert_eq "$(jqf "$out" .model)" "haiku" "valid selection: model echoed"
assert_eq "$(jqf "$out" .effort)" "max" "valid selection: effort echoed"
assert_eq "$(jqf "$out" .speed)" "fast" "valid selection: speed echoed"

# An unknown model for Claude refuses BEFORE claiming the story or creating a
# worktree -- mirroring the existing --agent validation's fail-fast contract.
repo2=$(mk_story_repo MDX)
id2=$(new_story "$repo2" "Unknown model refuses before side effects")
out=$(dispatch_real "$repo2" "$id2" --model=gpt-9)
assert_eq "$(jqf "$out" .ok)" "false" "unknown model: ok:false"
assert_contains "$out" "unknown model" "unknown model: names the problem"
assert_contains "$out" "opusplan" "unknown model: names a valid alternative"
state2=$(cd "$repo2" && story show "$id2" --json | jq -r '.story.story.state')
assert_eq "$state2" "todo" "unknown model: story was never claimed"
[ ! -d "$repo2/.claude/worktrees/$id2" ] || fail_test "unknown model: worktree was created anyway"

# An unknown effort likewise refuses before any side effect.
repo3=$(mk_story_repo MDE)
id3=$(new_story "$repo3" "Unknown effort refuses before side effects")
out=$(dispatch_real "$repo3" "$id3" --effort=ultra)
assert_eq "$(jqf "$out" .ok)" "false" "unknown effort: ok:false"
assert_contains "$out" "unknown effort" "unknown effort: names the problem"
state3=$(cd "$repo3" && story show "$id3" --json | jq -r '.story.story.state')
assert_eq "$state3" "todo" "unknown effort: story was never claimed"

# An unknown speed likewise refuses before any side effect.
repo4=$(mk_story_repo MDS)
id4=$(new_story "$repo4" "Unknown speed refuses before side effects")
out=$(dispatch_real "$repo4" "$id4" --speed=warp)
assert_eq "$(jqf "$out" .ok)" "false" "unknown speed: ok:false"
assert_contains "$out" "unknown speed" "unknown speed: names the problem"

# A model valid for Codex is invalid for Claude and vice versa -- the
# validation is provider-scoped, not a flat global list.
repo5=$(mk_story_repo MDC)
id5=$(new_story "$repo5" "Codex model rejected for Claude")
out=$(dispatch_real "$repo5" "$id5" --agent=claude --model=gpt-5.6-sol)
assert_eq "$(jqf "$out" .ok)" "false" "cross-provider model: ok:false"
assert_contains "$out" "unknown model" "cross-provider model: names the problem"

# Each flag may be specified only once, matching --agent's own contract.
repo6=$(mk_story_repo MDD)
id6=$(new_story "$repo6" "Duplicate model flag refused")
out=$(dispatch_real "$repo6" "$id6" --model=haiku --model=opus)
assert_eq "$(jqf "$out" .ok)" "false" "duplicate --model: ok:false"
assert_contains "$out" "--model may be specified only once" "duplicate --model: names the flag"

# STORY_MODEL/STORY_EFFORT/STORY_SPEED are the env-seam fallback beneath an
# explicit flag -- the same precedence STORY_AGENT already has beneath
# --agent (story.sh:1052-1059).
repo7=$(mk_story_repo MDV)
id7=$(new_story "$repo7" "STORY_MODEL env fallback")
out=$(cd "$repo7" \
  && PATH="$FAKE_TMUX_DIR:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" \
     STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
     STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
     STORY_MODEL=sonnet STORY_EFFORT=low STORY_SPEED=fast \
     bash "$SCRIPT" dispatch "$id7" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "env fallback: ok"
assert_eq "$(jqf "$out" .model)" "sonnet" "env fallback: STORY_MODEL used"
assert_eq "$(jqf "$out" .effort)" "low" "env fallback: STORY_EFFORT used"
assert_eq "$(jqf "$out" .speed)" "fast" "env fallback: STORY_SPEED used"

repo8=$(mk_story_repo MDF)
id8=$(new_story "$repo8" "explicit flag outranks STORY_MODEL")
out=$(cd "$repo8" \
  && PATH="$FAKE_TMUX_DIR:$PATH" TMUX="fake,0,0" TMUX_PANE="%0" \
     STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
     STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
     STORY_MODEL=sonnet \
     bash "$SCRIPT" dispatch "$id8" --model=haiku 2>&1)
assert_eq "$(jqf "$out" .model)" "haiku" "flag outranks env: explicit --model wins"

# An unselected dispatch reports no model/effort override and a standard
# speed -- today's behavior, unchanged.
repo9=$(mk_story_repo MDN)
id9=$(new_story "$repo9" "No selection: today's defaults")
out=$(dispatch_real "$repo9" "$id9")
assert_eq "$(jqf "$out" .model)" "opusplan" "no selection: reports the built-in default model"
assert_eq "$(jqf "$out" 'has("effort")')" "false" "no selection: no effort field"
assert_eq "$(jqf "$out" .speed)" "standard" "no selection: standard speed"

finish
