#!/usr/bin/env bash
# SH-655: /story do refuses to open a session past the machine lane budget,
# before any side effect, and only when it would ADD a session.
#
# D14 promises a machine-wide lane budget; until SH-655 only the engine read
# it, over its own lanes, and a dispatch typed by hand counted for nothing.
# cmd_dispatch now asks `story lane-budget --json` — a census of the live
# @storyhook-agent windows on THIS shell's tmux server — ahead of both modes'
# claim writes. The census here is the fake tmux's `$STATE/agent_windows`
# seed, in the verb's own format (session:window<TAB>agent<TAB>pane_dead).
#
# The budget is read back from the binary, never written as a literal here
# (SH-136): the seed is built to exactly that many live windows.
#
# STORY_DRY_RUN=1 is enough: the gate sits ahead of the claim, so a refusal
# never reaches a worktree or a window, and a permitted dry run reaches the
# preview branch (ok:true) without opening one either.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"
repo=$(mk_story_repo)

dry() {  # dry <story-id> [dispatch flags...]
  (cd "$repo" && PATH="$FAKE_TMUX_DIR:$PATH" \
    STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$@" 2>"$FAKE_TMUX_STATE/stderr")
}

budget=$(cd "$repo" && PATH="$FAKE_TMUX_DIR:$PATH" story lane-budget --json | jq -r '.budget')
[ "$budget" -ge 1 ] || fail_test "lane-budget: the binary reports no budget ($budget)"

# seed_live <n> [extra lines...] — n live tagged windows, plus any extra lines.
seed_live() {
  local n="$1"; shift
  : >"$FAKE_TMUX_STATE/agent_windows"
  local i=0
  while [ "$i" -lt "$n" ]; do
    printf 'storyhook:SH-9%02d\tclaude\t0\n' "$i" >>"$FAKE_TMUX_STATE/agent_windows"
    i=$((i + 1))
  done
  [ "$#" -eq 0 ] || printf '%s\n' "$@" >>"$FAKE_TMUX_STATE/agent_windows"
}

# --- at the budget: refused, pre-side-effect ---
id=$(new_story "$repo" "Budgeted story")
seed_live "$budget"
out=$(dry "$id")
assert_eq "$(jqf "$out" .ok)" "false" "at budget: ok:false"
assert_eq "$(jqf "$out" .reason)" "lane-budget" "at budget: reason is lane-budget"
assert_contains "$(jqf "$out" .display)" "budget of $budget" "at budget: display names the budget"
assert_contains "$(jqf "$out" .display)" "storyhook:SH-900" "at budget: display names a live session"
assert_contains "$(jqf "$out" .display)" "--over-budget" "at budget: display names the override"
assert_eq "$(jqf "$out" .lane_budget.live)" "$budget" "at budget: the census travels as data"
[ -d "$repo/.claude/worktrees" ] && fail_test "at budget: a worktree was created despite the refusal"
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "todo" "at budget: no claim was written"

# --- over the budget (more live than the ceiling) is refused the same way ---
seed_live $((budget + 2))
out=$(dry "$id")
assert_eq "$(jqf "$out" .reason)" "lane-budget" "over budget: still refused"

# --- --over-budget says you meant it: proceeds, and says so on stderr ---
seed_live "$budget"
out=$(dry "$id" --over-budget)
assert_eq "$(jqf "$out" .ok)" "true" "--over-budget: dispatch proceeds"
assert_contains "$(cat "$FAKE_TMUX_STATE/stderr")" "past the lane budget" "--over-budget: the override is reported, never silent"

# --- one below the budget: room for exactly this session ---
seed_live $((budget - 1))
out=$(dry "$id")
assert_eq "$(jqf "$out" .ok)" "true" "below budget: dispatch proceeds"
assert_contains "$(cat "$FAKE_TMUX_STATE/stderr")" "" "below budget: stderr is quiet"
grep -q 'lane budget' "$FAKE_TMUX_STATE/stderr" && fail_test "below budget: the gate spoke when nothing was at stake"

# --- a dead pane and an untagged window are not sessions ---
seed_live $((budget - 1)) $'storyhook:SH-DEAD\tclaude\t1' $'storyhook:zsh\t\t0' $'storyhook-verifier:verification\t\t0'
out=$(dry "$id")
assert_eq "$(jqf "$out" .ok)" "true" "dead pane and untagged windows: not counted"

# --- the engine's own lanes are not gated here: --full-auto skips the gate ---
seed_live "$budget"
out=$(dry "$id" --auto --full-auto)
assert_eq "$(jqf "$out" .ok)" "true" "--full-auto: the engine is the authority for its own lanes"

# --- an older binary that lacks the verb is no evidence, never a refusal ---
seed_live "$budget"
real_story=$(command -v story)
out=$(cd "$repo" && PATH="$TESTS_DIR/fakes/story-no-lane-budget:$FAKE_TMUX_DIR:$PATH" \
  STORY_REAL_BIN="$real_story" STORY_BIN="$TESTS_DIR/fakes/story-no-lane-budget/story" \
  STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" 2>"$FAKE_TMUX_STATE/stderr")
assert_eq "$(jqf "$out" .ok)" "true" "old binary: dispatch proceeds"
assert_contains "$(cat "$FAKE_TMUX_STATE/stderr")" "could not be measured" "old binary: the missing census is reported"

# --- an unanswered census (tmux cannot be asked) is reported, not refused ---
# The fake refuses to run at all without its state directory: that is the
# "tmux could not be asked" shape from the census's point of view.
seed_live "$budget"
out=$(cd "$repo" && PATH="$FAKE_TMUX_DIR:$PATH" FAKE_TMUX_STATE="$FAKE_TMUX_STATE/absent" \
  STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" 2>"$FAKE_TMUX_STATE/stderr")
assert_eq "$(jqf "$out" .ok)" "true" "unanswered: dispatch proceeds on no evidence"
assert_contains "$(cat "$FAKE_TMUX_STATE/stderr")" "could not be measured" "unanswered: reported on stderr"

# --- NEXT mode is gated too: the atomic claim is a write ---
seed_live "$budget"
out=$(dry --next)
assert_eq "$(jqf "$out" .reason)" "lane-budget" "--next: gated ahead of the atomic claim"
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "todo" "--next: nothing was claimed"

# --- an epic refuses --over-budget by name: its engine run has its own lanes ---
epic=$(cd "$repo" && story new "Epic" --type epic --json | jq -r '.story.story.id // .story.id // .id')
seed_live 0
out=$(dry "$epic" --auto --over-budget)
assert_eq "$(jqf "$out" .ok)" "false" "epic: --over-budget refused"
assert_contains "$(jqf "$out" .display)" "over-budget" "epic: the refusal names the flag"

finish
