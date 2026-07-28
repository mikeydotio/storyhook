#!/usr/bin/env bash
# SH-46 regression: every `story` call must run anchored at the project
# root, not in the caller's CWD.
#
# The story CLI has no --repo/-C/--cwd flag and ensure_project() does NOT
# walk up ancestors (src/storage.rs) -- the project root is always exactly
# env::current_dir(). So a dispatch that shells out to `story` from wherever
# the user happened to stand either fails outright (a plain subdirectory) or,
# far worse, silently reads a DIFFERENT tracker: `.storyhook/` is
# version-controlled, so every git worktree carries its own independent copy.
#
# repo_root() already anchors worktree CREATION to the main repo (it uses
# --git-common-dir deliberately, see its header). These assertions pin the
# other half: the story READS and the CAS claim must be anchored there too,
# or the gate is evaluated against one checkout while the worktree is made
# in another.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
ready_id=$(new_story "$repo" "Ready story")

# --- a plain subdirectory ---
mkdir -p "$repo/src/deep/nested"
out=$(cd "$repo/src/deep/nested" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$ready_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "subdir: dispatches from a nested subdirectory"
assert_eq "$(jqf "$out" .id)" "$ready_id" "subdir: resolved the right story"
assert_contains "$(jqf "$out" .display)" "$ready_id" "subdir: display names the story"

# Guard against a vacuous pass: prove the CLI really does refuse to run in
# that subdirectory on its own, so the assertion above can only be satisfied
# by story.sh anchoring the call itself.
sub_rc=0
(cd "$repo/src/deep/nested" && story show "$ready_id" --json >/dev/null 2>&1) || sub_rc=$?
assert_eq "$sub_rc" "3" "fixture sanity: bare \`story show\` in a subdir exits 3 (not initialized)"

# --- from inside a dispatched worktree (the silent-wrong-tracker case) ---
# Build a second checkout of the same repo. Its tracker is not the main
# repo's: `.storyhook/` is untracked in the fixture, so a linked worktree
# resolves no tracker there at all, and the story dispatched below exists only
# in the main repo's. If story.sh read from the worktree, the id would come
# back "not found" -- so a successful dispatch proves it read the main repo's.
only_in_main=$(new_story "$repo" "Only in the main checkout")
(cd "$repo" && git worktree add -q --no-track -b probe-wt "$repo/.claude/worktrees/probe" HEAD) >/dev/null 2>&1

out=$(cd "$repo/.claude/worktrees/probe" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$only_in_main" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "worktree: reads the MAIN repo's tracker, not the worktree's copy"
assert_eq "$(jqf "$out" .id)" "$only_in_main" "worktree: resolved the main-tracker-only story"

# Fixture sanity again: that story is genuinely absent from the worktree's
# own .storyhook/ copy, so the pass above cannot be an accident.
wt_rc=0
(cd "$repo/.claude/worktrees/probe" && story show "$only_in_main" --json >/dev/null 2>&1) || wt_rc=$?
[ "$wt_rc" -eq 0 ] && fail_test "fixture sanity: story $only_in_main should NOT resolve in the worktree's own tracker"

# The worktree path is still derived from the main repo, not nested inside
# the probe worktree we ran from. Compare against the PHYSICAL repo path:
# repo_root() resolves symlinks (`pwd -P`), and on macOS /tmp is a symlink
# to /private/tmp.
repo_phys=$(cd "$repo" && pwd -P)
assert_eq "$(jqf "$out" .worktree_path)" "$repo_phys/.claude/worktrees/$(jqf "$out" .window_name)" \
  "worktree: new worktree path is anchored to the main repo"

finish
