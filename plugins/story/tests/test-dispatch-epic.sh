#!/usr/bin/env bash
# Epics are planning containers. Bare named dispatch refuses them, while
# `--auto` starts the daemon-owned Full Auto engine on their descendants — no
# claim, worktree, tmux window, or direct provider handoff for the epic itself.
#
# SH-499: an epic is a story TYPED `epic`, never one that merely holds a
# `parent-of` edge. This fixture used to build its "structural epic" by giving
# an ordinary story a child and relying on that to confer epic-ness -- which is
# exactly the conflation SH-499 removed, so the fixture stated the defect. The
# epic is typed now, and the second half of this file asserts the other side:
# an ordinary story with a child is dispatchable, because it is work.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
epic=$(cd "$repo" && story new "Real epic" --type epic --json | jq -r '.story.story.id')
child=$(new_story "$repo" "Actionable child")
(cd "$repo" && story relate "$epic" parent-of "$child" >/dev/null)

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$epic" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "epic: ok:false"
assert_contains "$(jqf "$out" .display)" "is an epic" "epic: refusal names the structure"
assert_contains "$(jqf "$out" .display)" "Full Auto engine" "epic: refusal names the engine"
assert_contains "$(jqf "$out" .display)" "--auto" "epic: refusal gives the engine remedy"

forced=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$epic" --force 2>&1)
assert_eq "$(jqf "$forced" .ok)" "false" "forced epic: ok:false"
assert_contains "$(jqf "$forced" .display)" "Full Auto engine" "forced bare epic: still gives the engine remedy"

state=$(cd "$repo" && story show "$epic" --json | jq -r '.story.story.state')
assert_eq "$state" "todo" "epic: refused dispatch leaves effective state untouched"

# --- SH-517: a model/effort/speed selector does not reach the engine run ---
# LAUNCH_TPL, which --model/--effort/--speed compose into, is never consulted
# by an epic's engine-run path -- without this refusal the selector would
# validate cleanly and then be silently discarded rather than doing anything.
selector=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$epic" --auto --model=opus 2>&1)
assert_eq "$(jqf "$selector" .ok)" "false" "epic + --model: ok:false"
assert_contains "$(jqf "$selector" .display)" "is an epic" "epic + --model: names the structure"
assert_contains "$(jqf "$selector" .display)" "not yet supported" "epic + --model: names why"

# --- SH-469: --auto starts an epic-scoped run -------------------------------
# Dry-run is a distinct result variant and does no write. It also proves the
# adapter default reaches the engine request without requiring curl or tmux.
preview=$(
  cd "$repo" \
    && unset TMUX TMUX_PANE STORY_TARGET_SESSION \
    && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$epic" --auto 2>&1
)
assert_eq "$(jqf "$preview" .ok)" "true" "epic auto preview: ok:true"
assert_eq "$(jqf "$preview" .dry_run)" "true" "epic auto preview: dry_run:true"
assert_eq "$(jqf "$preview" .kind)" "engine-run" "epic auto preview: distinct result kind"
assert_eq "$(jqf "$preview" .epic)" "$epic" "epic auto preview: canonical scope"
assert_eq "$(jqf "$preview" .agent)" "claude" "epic auto preview: default agent"
assert_eq "$(jqf "$preview" .lanes)" "1" "epic auto preview: one lane"

# Epic-only modifiers are refused before the HTTP write, so neither can be
# mistaken for a story claim or the marker the engine puts on descendant lanes.
for flag in --force --full-auto; do
  refused=$(
    cd "$repo" \
      && unset TMUX TMUX_PANE STORY_TARGET_SESSION \
      && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$epic" --auto "$flag" 2>&1
  )
  assert_eq "$(jqf "$refused" .ok)" "false" "epic auto $flag: ok:false"
  assert_contains "$(jqf "$refused" .display)" "$flag" "epic auto $flag: refusal names the flag"
done

# The real path crosses SH-468's authenticated endpoint. It deliberately runs
# with no tmux variables: starting an engine is not dispatching the epic into a
# pane. Explicit provider selection is carried into the persisted run view.
started=$(
  cd "$repo" \
    && unset TMUX TMUX_PANE STORY_TARGET_SESSION \
    && bash "$SCRIPT" dispatch "$epic" --auto --agent=codex 2>&1
)
assert_eq "$(jqf "$started" .ok)" "true" "epic auto: ok:true"
assert_eq "$(jqf "$started" .kind)" "engine-run" "epic auto: distinct result kind"
assert_eq "$(jqf "$started" .epic)" "$epic" "epic auto: canonical scope"
assert_eq "$(jqf "$started" .agent)" "codex" "epic auto: selected agent"
assert_eq "$(jqf "$started" '.run.scope.kind')" "epic" "epic auto: persisted epic scope"
assert_eq "$(jqf "$started" '.run.scope.story')" "$epic" "epic auto: persisted scope id"
assert_eq "$(jqf "$started" '.run.agent')" "codex" "epic auto: persisted agent"
assert_eq "$(jqf "$started" '.run.lane_count')" "1" "epic auto: persisted lane default"
assert_eq "$(jqf "$started" '.claimed // "absent"')" "absent" "epic auto: no story claim result"
assert_eq "$(jqf "$started" '.worktree_path // "absent"')" "absent" "epic auto: no worktree result"
[ ! -e "$repo/.claude/worktrees/$epic" ] \
  || fail_test "epic auto: created a Claude worktree for the epic"
[ ! -e "$repo/.codex/worktrees/$epic" ] \
  || fail_test "epic auto: created a Codex worktree for the epic"

# The first real call succeeding proves the preceding bare refusal and dry-run
# did not create a run. A second real call reaches the service's unique-live-run
# guard and is carried back through the helper's structured refusal.
duplicate=$(
  cd "$repo" \
    && unset TMUX TMUX_PANE STORY_TARGET_SESSION \
    && bash "$SCRIPT" dispatch "$epic" --auto --agent=codex 2>&1
)
assert_eq "$(jqf "$duplicate" .ok)" "false" "duplicate epic auto: ok:false"
assert_eq "$(jqf "$duplicate" .reason)" "engine-start-refused" \
  "duplicate epic auto: structured service refusal"
assert_contains "$(jqf "$duplicate" .display)" "already has a live engine run" \
  "duplicate epic auto: service diagnosis survives"

# --- the other side of SH-499 -------------------------------------------------
# A NORMAL story that happens to have a child is ordinary work, and dispatch
# must offer it. Before SH-499 this refused for the same reason the epic above
# does, so a bug that spawned a follow-up became permanently undispatchable.
parent=$(new_story "$repo" "A normal story with a sub-task")
subtask=$(new_story "$repo" "Its sub-task")
(cd "$repo" && story relate "$parent" parent-of "$subtask" >/dev/null)

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$parent" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "normal parent: dispatchable, because it is work"
case "$(jqf "$out" .display)" in
  *"is an epic"*) fail_test "normal parent: refused as an epic for having a child" ;;
esac

finish
