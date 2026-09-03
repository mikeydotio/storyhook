#!/usr/bin/env bash
# SH-46 regression: every `story` call must run anchored at the project
# root, not in the caller's CWD.
#
# Both halves of SH-46 are fixed now, and this file pins both.
#
# It used to be a defence: the CLI resolved a project from exactly its working
# directory, so a dispatch that shelled out to `story` from wherever the user
# stood either failed outright (a plain subdirectory) or, far worse, silently
# read a DIFFERENT tracker, because `.storyhook/` was version-controlled and
# every git worktree carried its own independent copy.
#
# It is now mostly a proof. The CLI walks up from its working directory, and
# every checkout of a repository resolves to one project in one store — so the
# assertions below check that the *right* thing happens for the *right*
# reason, including the two that state the property directly: a bare `story
# show` succeeds in a subdirectory, and a story created in the main checkout is
# visible from a linked worktree.
#
# What story.sh's own anchoring still buys is the worktree bookkeeping:
# repo_root() anchors worktree CREATION to the main repo (it uses
# --git-common-dir deliberately, see its header), so a new worktree is never
# nested inside the one it was dispatched from.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# Dispatch inventory is read-only even under STORY_DRY_RUN, but it still asks
# tmux whether this story already owns a window. Keep this cwd test independent
# of operator sessions whose window names can collide with its fixed TST ids.
dry_dispatch() {
  local dir="$1" id="$2"
  (
    cd "$dir" &&
      PATH="$FAKE_TMUX_DIR:$PATH" FAKE_TMUX_PANES= STORY_DRY_RUN=1 \
        bash "$SCRIPT" dispatch "$id" 2>&1
  )
}

repo=$(mk_story_repo)
ready_id=$(new_story "$repo" "Ready story")

# --- a plain subdirectory ---
mkdir -p "$repo/src/deep/nested"
out=$(dry_dispatch "$repo/src/deep/nested" "$ready_id")
assert_eq "$(jqf "$out" .ok)" "true" "subdir: dispatches from a nested subdirectory"
assert_eq "$(jqf "$out" .id)" "$ready_id" "subdir: resolved the right story"
assert_contains "$(jqf "$out" .display)" "$ready_id" "subdir: display names the story"

# The CLI resolves a project by walking up from the working directory now, so
# a bare `story show` in a subdirectory succeeds where it used to exit 3. That
# is the behaviour change the data-layer rearchitecture makes on purpose, and
# it is why story.sh's own anchoring is belt-and-braces rather than the only
# thing holding this up. Asserted rather than dropped: if the walk ever
# regresses, the assertion above would start passing for the old reason.
sub_rc=0
(cd "$repo/src/deep/nested" && story show "$ready_id" --json >/dev/null 2>&1) || sub_rc=$?
assert_eq "$sub_rc" "0" "the CLI resolves the project by walking up from a subdirectory"

# --- from inside a dispatched worktree ---
# Build a second checkout of the same repo. Both checkouts see one project now
# — that is the headline property of the rearchitecture — so a dispatch from
# the worktree resolves the story either way. What is still worth asserting is
# that the *worktree path* it computes is anchored to the main repo rather than
# nested inside the worktree it was run from.
only_in_main=$(new_story "$repo" "Only in the main checkout")
(cd "$repo" && git worktree add -q --no-track -b probe-wt "$repo/.claude/worktrees/probe" HEAD) >/dev/null 2>&1

out=$(dry_dispatch "$repo/.claude/worktrees/probe" "$only_in_main")
assert_eq "$(jqf "$out" .ok)" "true" "worktree: reads the MAIN repo's tracker, not the worktree's copy"
assert_eq "$(jqf "$out" .id)" "$only_in_main" "worktree: resolved the main-tracker-only story"

# The inverse of what this used to assert. A story created in the main checkout
# is *visible* from a linked worktree, because they are one project — the exact
# divergence (SH-46) the global store was built to end.
wt_rc=0
(cd "$repo/.claude/worktrees/probe" && story show "$only_in_main" --json >/dev/null 2>&1) || wt_rc=$?
assert_eq "$wt_rc" "0" "a story created in the main checkout resolves from a linked worktree"

# The worktree path is still derived from the main repo, not nested inside
# the probe worktree we ran from. Compare against the PHYSICAL repo path:
# repo_root() resolves symlinks (`pwd -P`), and on macOS /tmp is a symlink
# to /private/tmp.
repo_phys=$(cd "$repo" && pwd -P)
assert_eq "$(jqf "$out" .worktree_path)" "$repo_phys/.claude/worktrees/$(jqf "$out" .window_name)" \
  "worktree: new worktree path is anchored to the main repo"

finish
