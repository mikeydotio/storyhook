#!/usr/bin/env bash
# SH-166 dropped the `<repo-prefix>-` stem from every window/worktree/branch
# name story.sh derives (session.sh's resolve_wname is now bare-id-only), but
# a story dispatched by a PRE-SH-166 binary already has a worktree and branch
# on disk under the old scheme. Without a fallback, `complete` would report
# the canonical (bare-id) name "missing"/"missing", close the story, and
# silently strand that worktree and branch on disk -- the regression this
# file exists to pin.
#
# lib.sh's legacy_wname_for/mk_dispatched_legacy build fixtures under the OLD
# scheme; wname_for/mk_dispatched (unchanged names, new bare-id behavior)
# build the current one. Every scenario below constructs whichever of the
# two (or both) it needs and asserts adoption via disk state -- not just the
# JSON's `legacy_name` flag -- exactly like test-complete-execute.sh's own
# survivor assertions.
source "$(dirname "$0")/lib.sh"

# capture's readiness probe shells out to `tmux` directly (no dispatch_real
# wrapper here) -- point PATH at the fake before any raw `capture` call, same
# as test-doctor-capture.sh.
export PATH="$TESTS_DIR/fakes:$PATH"

repo=$(mk_story_repo)

# --- complete plan adopts a legacy worktree + branch when no canonical one exists ---
id=$(new_story "$repo" "Legacy only")
legacy=$(mk_dispatched_legacy "$repo" "$id")
[ "$legacy" != "$id" ] || fail_test "legacy fixture: prefix collapsed to the bare id, nothing to prove"
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "adopt-plan: ok"
assert_eq "$(jqf "$out" .legacy_name)" "true" "adopt-plan: flags the adoption"
assert_eq "$(basename "$(jqf "$out" .plan.worktree.path)")" "$legacy" \
  "adopt-plan: targets the legacy worktree, not the (nonexistent) canonical one"
assert_eq "$(jqf "$out" .plan.worktree.status)" "removable" "adopt-plan: classified, not reported missing"
assert_eq "$(jqf "$out" .plan.branch.name)" "worktree-$legacy" "adopt-plan: targets the legacy branch"
assert_eq "$(jqf "$out" .plan.branch.status)" "deletable" "adopt-plan: legacy branch classified as deletable"
assert_eq "$(jqf "$out" .actions_count)" "3" "adopt-plan: close + worktree + branch all counted"
assert_contains "$(jqf "$out" .display)" "pre-SH-166" "adopt-plan: display names it as the legacy form"

# --- complete execute actually removes the adopted legacy worktree and branch ---
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "adopt-exec: ok"
assert_eq "$(jqf "$out" .legacy_name)" "true" "adopt-exec: flags the adoption"
assert_eq "$(jqf "$out" .closed)" "true" "adopt-exec: story closed"
assert_eq "$(jqf "$out" '.removed.worktrees|length')" "1" "adopt-exec: one worktree removed"
assert_eq "$(jqf "$out" '.removed.branches|length')" "1" "adopt-exec: one branch removed"
[ -d "$repo/.claude/worktrees/$legacy" ] && fail_test "adopt-exec: legacy worktree still on disk -- this is the silent-litter regression"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$legacy") \
  && fail_test "adopt-exec: legacy branch still in git -- this is the silent-litter regression"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.superstate')" "CLOSED" \
  "adopt-exec: story really closed per the real CLI"

# --- a canonical worktree/branch is never overridden when both exist ---
both=$(new_story "$repo" "Both schemes")
mk_dispatched_legacy "$repo" "$both" >/dev/null
canonical=$(mk_dispatched "$repo" "$both")
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$both" 2>&1)
assert_eq "$(jqf "$out" .legacy_name)" "false" "both-exist: canonical wins, no adoption"
assert_eq "$(basename "$(jqf "$out" .plan.worktree.path)")" "$canonical" \
  "both-exist: targets the canonical worktree"
assert_eq "$(jqf "$out" .plan.branch.name)" "worktree-$canonical" "both-exist: targets the canonical branch"
# Cleanup targets only the canonical pair -- the legacy one from before must survive untouched.
legacy_both=$(legacy_wname_for "$repo" "$both")
out=$(cd "$repo" && bash "$SCRIPT" complete execute "$both" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "both-exist exec: ok"
[ -d "$repo/.claude/worktrees/$legacy_both" ] \
  || fail_test "both-exist exec: the untargeted legacy worktree was removed anyway"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$legacy_both") \
  || fail_test "both-exist exec: the untargeted legacy branch was deleted anyway"

# --- dispatch refuses against a legacy-only worktree, and rolls the claim back ---
FAKE_TMUX_DIR="$TESTS_DIR/fakes"
dispatch_real() {
  local dir="$1" sid="$2"
  (
    cd "$dir" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch "$sid" 2>&1
  )
}
dsp=$(new_story "$repo" "Already dispatched under the old scheme")
legacy_dsp=$(mk_dispatched_legacy "$repo" "$dsp")
out=$(dispatch_real "$repo" "$dsp")
assert_eq "$(jqf "$out" .ok)" "false" "dispatch-legacy-collision: ok:false"
assert_contains "$(jqf "$out" .display)" "$legacy_dsp" "dispatch-legacy-collision: names the legacy worktree"
assert_contains "$(jqf "$out" .display)" "pre-SH-166" "dispatch-legacy-collision: says it's the pre-SH-166 name"
assert_contains "$(jqf "$out" .display)" "Rolled the claim back" "dispatch-legacy-collision: claim was rolled back"
[ -d "$repo/.claude/worktrees/$dsp" ] \
  && fail_test "dispatch-legacy-collision: a SECOND (canonical) worktree was created anyway"
rolled=$(cd "$repo" && story show "$dsp" --json | jq -r '.story.story.state')
assert_eq "$rolled" "todo" "dispatch-legacy-collision: story not stranded at in-progress"

# --- capture falls back to a live legacy tmux window ---
cap=$(new_story "$repo" "Capture legacy")
legacy_cap=$(legacy_wname_for "$repo" "$cap")
out=$(cd "$repo" && TMUX=fake TMUX_PANE=%0 \
       FAKE_TMUX_PANES="$(printf 'other\t1\t%%3\n%s\t1\t%%9' "$legacy_cap")" \
       FAKE_TMUX_TRANSCRIPT="legacy session output" \
       bash "$SCRIPT" capture "$cap" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "capture-legacy: ok"
assert_eq "$(jqf "$out" .window_name)" "$legacy_cap" "capture-legacy: reports the legacy window it actually found"
assert_eq "$(jqf "$out" .legacy_name)" "true" "capture-legacy: flags the adoption"
assert_eq "$(jqf "$out" .pane)" "%9" "capture-legacy: resolves the pane of the legacy window"
assert_contains "$(jqf "$out" .transcript)" "legacy session output" "capture-legacy: returns the transcript"

# --- nothing dispatched at all: no false adoption ---
bare=$(new_story "$repo" "Never dispatched at all")
out=$(cd "$repo" && bash "$SCRIPT" complete plan "$bare" 2>&1)
assert_eq "$(jqf "$out" .legacy_name)" "false" "never-dispatched: no adoption invented from nothing"
assert_eq "$(jqf "$out" .plan.worktree.status)" "missing" "never-dispatched: worktree genuinely missing"
assert_eq "$(jqf "$out" .plan.branch.status)" "missing" "never-dispatched: branch genuinely missing"
cap_out=$(cd "$repo" && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="" bash "$SCRIPT" capture "$bare" 2>&1)
assert_eq "$(jqf "$cap_out" .ok)" "false" "never-dispatched capture: no live window, canonical or legacy"
assert_contains "$(jqf "$cap_out" .display)" "$bare" "never-dispatched capture: names the canonical form"

# --- STORY_WINDOW_NAME override: no legacy candidate, so no adoption ---
ov=$(new_story "$repo" "Override no adoption")
legacy_ov=$(mk_dispatched_legacy "$repo" "$ov")
out=$(cd "$repo" && STORY_WINDOW_NAME="custom-<n>" bash "$SCRIPT" complete plan "$ov" 2>&1)
assert_eq "$(jqf "$out" .legacy_name)" "false" "override: an explicit override never adopts a legacy name"
assert_eq "$(basename "$(jqf "$out" .plan.worktree.path)")" "custom-$ov" \
  "override: targets the override's own name, ignoring the legacy worktree on disk"
[ -d "$repo/.claude/worktrees/$legacy_ov" ] \
  || fail_test "override: the pre-existing legacy worktree vanished just from being asked about"

finish
