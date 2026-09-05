#!/usr/bin/env bash
# A bare story id (`5`) reaches this script the moment the CLI resolves one
# (SH-118), and everything this script *names* -- the tmux window, the worktree
# directory leaf, the branch -- must come out identical whichever form was
# typed. Otherwise `/story do 5` and `/story do SH-5` build two worktrees for
# one story, and `/story capture 5` hunts for a window no dispatch created.
#
# The sharper half is the ready gate: it tests membership against
# `story list --ready --json`, whose ids are canonical, so a bare id that only
# passed `story show` would be reported as *not ready* -- a wrong answer about a
# story that is perfectly ready.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

dispatch_real() {
  local dir="$1" id="$2"
  (
    cd "$dir" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch "$id" 2>&1
  )
}

repo=$(mk_story_repo)
id=$(new_story "$repo" "Dispatched by its number alone")
number="${id##*-}"

# Premise: the fixture really did mint a hyphenated id, so `$number` is a
# genuinely different string from `$id`. Without this the test could pass while
# proving nothing.
[ "$number" != "$id" ] || fail_test "bare-id: fixture minted no prefix, so there is nothing to resolve"

out=$(dispatch_real "$repo" "$number")
assert_eq "$(jqf "$out" .ok)" "true" "bare-id: dispatching by number succeeds"
assert_eq "$(jqf "$out" .id)" "$id" "bare-id: the response reports the canonical id"
assert_eq "$(jqf "$out" .window_name)" "$id" "bare-id: the window is named from the canonical id"
assert_eq "$(jqf "$out" '.cleanup_lease.version')" "1" \
  "bare-id: dispatch returns the versioned cleanup lease"
assert_eq "$(jqf "$out" '.cleanup_lease.project_slug')" "$(slug_for "$repo")" \
  "bare-id: cleanup lease names the selected project"
assert_eq "$(jqf "$out" '.cleanup_lease.story_id')" "$id" \
  "bare-id: cleanup lease names the canonical story"
assert_eq "$(jqf "$out" '.cleanup_lease.worktree_path')" "$(jqf "$out" .worktree_path)" \
  "bare-id: cleanup lease preserves the creation-time worktree"
assert_eq "$(jqf "$out" '.cleanup_lease.branch')" "worktree-$id" \
  "bare-id: cleanup lease preserves the creation-time branch"
assert_contains "$(jqf "$out" '.cleanup_lease.tmux.socket_path')" "/" \
  "bare-id: cleanup lease carries an absolute tmux socket"

# The names on disk are the ones a later `/story do SH-N` would collide with,
# which is the whole point of asserting them rather than trusting the JSON.
[ -d "$repo/.claude/worktrees/$id" ] \
  || fail_test "bare-id: worktree directory is not the one the canonical id names"
[ -d "$repo/.claude/worktrees/$number" ] \
  && fail_test "bare-id: a second worktree was created under the bare id"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$id") \
  || fail_test "bare-id: worktree branch is not the one the canonical id names"

claimed_state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$claimed_state" "in-progress" "bare-id: the claim landed on the story itself"

# `view` reports the canonical id too, so an agent that pipes one verb into
# another never carries the bare form forward.
view_out=$(cd "$repo" && bash "$SCRIPT" view "$number" 2>&1)
assert_eq "$(jqf "$view_out" .id)" "$id" "bare-id: view reports the canonical id"

# `capture` finds the window `dispatch` created, given the other form.
capture_out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" capture "$number" 2>&1)
assert_eq "$(jqf "$capture_out" .window_name)" "$id" \
  "bare-id: capture looks for the window dispatch actually opened"

# `complete plan` classifies the same worktree, so cleanup cannot miss it.
# Compared by leaf rather than by whole path: `repo_root` resolves symlinks and
# the fixture root may not be spelled the way this test spelled it.
plan_out=$(cd "$repo" && bash "$SCRIPT" complete plan "$number" 2>&1)
assert_eq "$(jqf "$plan_out" .id)" "$id" "bare-id: complete plan reports the canonical id"
assert_eq "$(basename "$(jqf "$plan_out" .plan.worktree.path)")" "$id" \
  "bare-id: complete plan found the real worktree"
assert_eq "$(jqf "$plan_out" .plan.worktree.status)" "removable" \
  "bare-id: and classified it, rather than reporting it missing"
assert_eq "$(jqf "$plan_out" .plan.branch.name)" "worktree-$id" \
  "bare-id: complete plan found the real branch"

finish
