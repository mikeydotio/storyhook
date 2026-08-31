#!/usr/bin/env bash
# Issue #40 gap this fork exists to close: storyhook's own is_ready()
# returns true even for a story already in-progress -- it only checks
# open/unblocked, not "hasn't been claimed yet". /story do must still
# refuse a redispatch, via an explicit guard checked BEFORE the ready-gate.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Already claimed")
(cd "$repo" && story move "$id" in-progress >/dev/null)

# Fixture sanity: storyhook's SH-236 fix (`is_claimable`, layered on top of
# `is_ready`) now excludes an in-progress story from `story list --ready`
# itself, so the already-in-progress guard below is no longer the only thing
# that would refuse this redispatch -- the ready-gate (deviation #2, step 6)
# would too, just with a more generic reason ("not in `story list --ready`")
# than this guard's specific one. The guard stays: its message names the
# likely cause and gives a concrete unstick command, which the ready-gate's
# fallback reason does not. This assertion pins the new ground truth so a
# future storyhook regression here is caught by *this* test rather than only
# by storyhook's own SH-236 regression tests.
still_ready=$(cd "$repo" && story list --ready --json 2>/dev/null | jq --arg id "$id" '([.stories[]?.story.id // empty] | index($id)) != null')
assert_eq "$still_ready" "false" "fixture sanity: an in-progress story is excluded from \`story list --ready\` (SH-236)"

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "already-in-progress: ok:false"
assert_eq "$(jqf "$out" .reason)" "resume-available" \
  "already-in-progress: active claim is offered as recoverable context"
assert_eq "$(jqf "$out" .resources.claim)" "present" \
  "already-in-progress: inventory reports the surviving claim"
assert_contains "$(jqf "$out" .display)" "in-progress" \
  "already-in-progress: reason names the active-role state"

# No worktree created, no additional state churn.
[ -d "$repo/.claude/worktrees" ] && fail_test "already-in-progress: a worktree was created despite the refusal"
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "in-progress" "already-in-progress: state unchanged (still in-progress, not re-moved)"

# SH-440: --force is an explicit escape from this ONE guard. Dry-run proves
# the planned command list contains no redundant state move; the real fake-tmux
# dispatch below proves the rest of the actuator still runs and the exported
# event history remains unchanged.
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch --force "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "forced dry-run: ok:true"
assert_eq "$(jqf "$out" .forced)" "true" "forced dry-run: forced:true"
assert_eq "$(jqf "$out" .reused_claim)" "true" "forced dry-run: reused_claim:true"
assert_eq "$(jqf "$out" .claim_transitioned)" "false" "forced dry-run: claim_transitioned:false"
assert_contains "$(jqf "$out" .display)" "reuse its existing" "forced dry-run: display names claim reuse"
# SH-482 re-spelled the claim as `story claim <id>`. Retargeted rather than
# left alone: a negative case that names a command the script can no longer
# emit is a filter nothing can match, which passes forever and proves nothing.
commands=$(jqf "$out" '.commands | join("\n")')
case "$commands" in
  *"story claim $id"* | *"story move $id in-progress"*)
    fail_test "forced dry-run: command list contains a redundant state transition" ;;
esac

transition_count() {
  (cd "$repo" && story export) \
    | jq --arg id "$id" '[.stories[] | select(.id == $id).events[] | select(.kind == "StoryStateChanged")] | length'
}
before=$(transition_count)

out=$(
  cd "$repo" \
    && PATH="$TESTS_DIR/fakes:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id" --force 2>&1
)
assert_eq "$(jqf "$out" .ok)" "true" "forced dispatch: ok:true"
assert_eq "$(jqf "$out" .claimed)" "true" "forced dispatch: claimed state is retained"
assert_eq "$(jqf "$out" .forced)" "true" "forced dispatch: forced:true"
assert_eq "$(jqf "$out" .reused_claim)" "true" "forced dispatch: reused_claim:true"
assert_eq "$(jqf "$out" .claim_transitioned)" "false" "forced dispatch: no transition owned by this dispatch"
assert_contains "$(jqf "$out" .display)" "no state transition" "forced dispatch: display names no-write behavior"

after=$(transition_count)
assert_eq "$after" "$before" "forced dispatch: exported state-transition count is unchanged"
[ -d "$repo/.claude/worktrees/$id" ] \
  || fail_test "forced dispatch: worktree was not created"

# A failure after the forced reuse must clean up only the side effects this
# dispatch owns. In particular, it must neither roll back nor misreport the
# pre-existing claim as absent.
repo_failed=$(mk_story_repo FRD)
id_failed=$(new_story "$repo_failed" "Failed forced redispatch")
(cd "$repo_failed" && story move "$id_failed" in-progress >/dev/null)
out=$(
  cd "$repo_failed" \
    && PATH="$TESTS_DIR/fakes:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_LAUNCH_MANGLE=1 \
      bash "$SCRIPT" dispatch "$id_failed" --force 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "failed forced dispatch: ok:false"
assert_eq "$(jqf "$out" .reason)" "pane-not-ready" "failed forced dispatch: later safety gate still applies"
assert_eq "$(jqf "$out" .claimed)" "true" "failed forced dispatch: pre-existing claim is reported retained"
assert_contains "$(jqf "$out" .display)" "pre-existing" "failed forced dispatch: display names retained claim"
failed_state=$(cd "$repo_failed" && story show "$id_failed" --json | jq -r '.story.story.state')
assert_eq "$failed_state" "in-progress" "failed forced dispatch: pre-existing claim remains in-progress"
[ ! -d "$repo_failed/.claude/worktrees/$id_failed" ] \
  || fail_test "failed forced dispatch: owned worktree was not rolled back"

finish
