#!/usr/bin/env bash
# SH-344's NEXT MODE (`story.sh dispatch --next`): the grammar refusals and
# the empty-ready-set refusal. All refuse BEFORE any side effect — no
# worktree, no tmux window, no claim — so STORY_DRY_RUN=1 is enough for the
# grammar cases and the empty-ready-set case alike (the latter refuses
# before the dry-run/real-run branch point, exactly like the ID MODE
# ready-gate refusals in test-dispatch-ready-gate.sh).
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- --next combined with an explicit id is a usage failure, either order ---
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch SH-1 --next 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "next+id (id first): ok:false"
assert_contains "$(jqf "$out" .display)" "--next cannot be combined with a story id" "next+id (id first): names the conflict"

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch --next SH-1 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "next+id (--next first): ok:false"
assert_contains "$(jqf "$out" .display)" "--next cannot be combined with a story id" "next+id (--next first): names the conflict"

# --- neither an id nor --next is a usage failure ---
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "no id, no --next: ok:false"
assert_contains "$(jqf "$out" .display)" "usage: story.sh dispatch" "no id, no --next: usage message"

# --- nothing ready yet (this repo has no stories at all): refuses cleanly ---
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch --next 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "nothing ready: ok:false"
assert_contains "$(jqf "$out" .display)" "no ready stories" "nothing ready: names the reason"
[ -d "$repo/.claude/worktrees" ] && fail_test "nothing ready: a worktree was created despite nothing to claim"

# --- now seed a ready story for the remaining, positive-path checks ---
id=$(new_story "$repo" "Ready for dry-run preview")

# --agent and --auto compose with --next in arbitrary order once there is
# something to claim.
out=$(cd "$repo" && STORY_AGENT=unknown STORY_DRY_RUN=1 \
  bash "$SCRIPT" dispatch --agent=codex --auto --next 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "next+agent+auto: accepted once a ready story exists"
assert_eq "$(jqf "$out" .agent)" "codex" "next+agent+auto: explicit provider carried through NEXT MODE"
assert_eq "$(jqf "$out" .auto)" "true" "next+agent+auto: auto:true carried through NEXT MODE same as ID MODE"
assert_contains "$(jqf "$out" .worktree_path)" "/.codex/worktrees/" \
  "next+agent+auto: provider worktree carried through NEXT MODE"

# --- dry-run claims nothing: the preview names the story but does not move it ---
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch --next 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry-run: ok:true"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry-run: dry_run:true"
assert_eq "$(jqf "$out" .id)" "$id" "dry-run: previews the story --next would pick"
assert_eq "$(jqf "$out" .state)" "todo" "dry-run: state shown is the PRE-claim state — nothing was written"
assert_eq "$(jqf "$out" '.commands[0]')" "story claim --next --no-comment" "dry-run: the claim command previewed is claim --next, not move --if-state"
assert_contains "$(jqf "$out" .display)" 'would claim it via `story claim --next --no-comment`' "dry-run: display names the next-mode claim command"
[ -d "$repo/.claude/worktrees" ] && fail_test "dry-run: a worktree was created despite dry-run"
untouched_state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$untouched_state" "todo" "dry-run: the story CLI itself confirms nothing was claimed"

finish
