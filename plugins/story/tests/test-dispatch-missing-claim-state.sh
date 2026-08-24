#!/usr/bin/env bash
# SH-440: a failed claim caused by a missing literal `in-progress` state is a
# permanent, operator-fixable configuration error. Confirm it from the actual
# vocabulary, return a distinct reason and remedy, and degrade to the original
# generic move failure whenever that confirmation cannot be trusted.
source "$(dirname "$0")/lib.sh"

dispatch_without_tmux() {
  local repo="$1" id="$2"
  shift 2
  (
    cd "$repo" \
      && PATH="$TESTS_DIR/fakes/story-state-list:$PATH" \
        STORY_REAL_BIN="$real_story" STORY_CLAIM_STATE_MISSING=1 \
        STORY_TARGET_SESSION=test bash "$SCRIPT" dispatch "$id" "$@" 2>&1
  )
}

real_story=$(command -v story)
fake_dir="$TESTS_DIR/fakes/story-state-list"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Missing claim state")

out=$(STORY_STATE_LIST_MODE=missing dispatch_without_tmux "$repo" "$id")
assert_eq "$(jqf "$out" .ok)" "false" "missing state: ok:false"
assert_eq "$(jqf "$out" .reason)" "claim-state-missing" "missing state: distinct reason"
assert_contains "$(jqf "$out" .display)" "in-progress" "missing state: names the absent state"
assert_contains "$(jqf "$out" .display)" "story doctor --fix" "missing state: names the remedy"
assert_contains "$(jqf "$out" .display)" "is not defined" "missing state: preserves the CLI error"
assert_contains "$(jqf "$out" .display)" "todo, blocked, done" "missing state: reports observed vocabulary"
[ -d "$repo/.claude/worktrees" ] \
  && fail_test "missing state: a worktree was created despite the failed claim"

repo_fail=$(mk_story_repo FLR)
id_fail=$(new_story "$repo_fail" "Unconfirmable missing state")
out=$(
  cd "$repo_fail" \
    && PATH="$fake_dir:$PATH" STORY_REAL_BIN="$real_story" STORY_CLAIM_STATE_MISSING=1 \
      STORY_STATE_LIST_MODE=fail \
      STORY_TARGET_SESSION=test bash "$SCRIPT" dispatch "$id_fail" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "false" "state-list failure: ok:false"
assert_eq "$(jqf "$out" .reason)" "null" "state-list failure: no unproven classification"
assert_contains "$(jqf "$out" .display)" "is not defined" "state-list failure: original error survives"

repo_empty=$(mk_story_repo EMP)
id_empty=$(new_story "$repo_empty" "Empty state list")
out=$(
  cd "$repo_empty" \
    && PATH="$fake_dir:$PATH" STORY_REAL_BIN="$real_story" STORY_CLAIM_STATE_MISSING=1 \
      STORY_STATE_LIST_MODE=empty \
      STORY_TARGET_SESSION=test bash "$SCRIPT" dispatch "$id_empty" 2>&1
)
assert_eq "$(jqf "$out" .reason)" "null" "empty state list: no unproven classification"

repo_space=$(mk_story_repo SPC)
id_space=$(new_story "$repo_space" "Whitespace state list")
out=$(
  cd "$repo_space" \
    && PATH="$fake_dir:$PATH" STORY_REAL_BIN="$real_story" STORY_CLAIM_STATE_MISSING=1 \
      STORY_STATE_LIST_MODE=whitespace \
      STORY_TARGET_SESSION=test bash "$SCRIPT" dispatch "$id_space" 2>&1
)
assert_eq "$(jqf "$out" .reason)" "null" "whitespace state list: no unproven classification"

# The confirming read belongs only to the already-failed claim path. A normal
# successful dispatch through the same proxy must never call `state list`.
repo_happy=$(mk_story_repo HAP)
id_happy=$(new_story "$repo_happy" "Successful claim")
state_log=$(mktemp /tmp/story-state-list-log.XXXXXX)
: >"$state_log"
out=$(
  cd "$repo_happy" \
    && PATH="$fake_dir:$TESTS_DIR/fakes:$PATH" STORY_REAL_BIN="$real_story" \
      STORY_STATE_LIST_MODE=fail STORY_STATE_LIST_LOG="$state_log" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id_happy" 2>&1
)
assert_eq "$(jqf "$out" .ok)" "true" "successful claim: ok:true"
[ ! -s "$state_log" ] || fail_test "successful claim: paid for a failure-only state-list read"

# The classifier necessarily reads Storyhook's current human-rendered state
# list because the JSON response does not expose structured state rows. Pin the
# two upstream premises against this repository's real CLI: the message names
# `in-progress`, and each state slug is its line's first token.
repo_premise=$(mk_story_repo PRM)
state_json=$(cd "$repo_premise" && story state list --json)
state_message=$(jqf "$state_json" '.message // ""')
assert_contains "$state_message" "in-progress" "real state list: message names the claim state"
state_slugs=$(printf '%s\n' "$state_message" | awk 'NF { printf "%s%s", (n++ ? ", " : ""), $1 }')
assert_contains "$state_slugs" "in-progress" "real state list: claim-state slug is the first token"
assert_contains "$state_slugs" "todo" "real state list: parser recovers sibling states"

finish
