#!/usr/bin/env bash
# SH-120: the three repo-side verbs take their directory from the project's
# LINKED CHECKOUT, never from the caller's working directory.
#
# The filed defect is "dispatch derives its directory from CWD". The real one is
# wider: every git call in story.sh and lib/session.sh was bare — no `-C`, no
# subshell `cd` — so each acted on whatever repository the caller happened to be
# standing in. Changing only the reported `dir` would leave `git worktree add`
# cutting a worktree of the CALLER's repo at a path inside the project's
# directory: strictly worse than the defect as filed.
#
# So this file asserts the reported path AND probes the git calls behind it. The
# collision probe below is the one that tells those two apart: it pre-creates a
# colliding branch in project A ONLY, and requires dispatch to SEE it from B. A
# fix that corrected `dir` while leaving `git show-ref` unscoped passes every
# path assertion here and fails that one.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# dispatch_dry <cwd> <args...> — a dry-run dispatch, run from <cwd>.
dispatch_dry() {
  local cwd="$1"
  shift
  (cd "$cwd" && STORY_DRY_RUN=1 bash "$SCRIPT" "$@" 2>&1)
}

# dispatch_real <cwd> <args...> — a real dispatch against the fake tmux, run
# from <cwd>. Needed by the collision probe: the worktree/branch collision check
# lives past the dry-run return.
dispatch_real() {
  local cwd="$1"
  shift
  (
    cd "$cwd" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" "$@" 2>&1
  )
}

repo_a=$(mk_story_repo AAA)
repo_b=$(mk_story_repo BBB)
a_phys=$(cd "$repo_a" && pwd -P)
b_phys=$(cd "$repo_b" && pwd -P)
slug_a=$(slug_for "$repo_a")
nowhere=$(mktemp -d /tmp/story-test-nowhere.XXXXXX)
_TMP_REPOS+=("$nowhere")

# --- dispatch lands in the project's checkout, not the caller's repo --------

id=$(new_story "$repo_a" "Dispatched from the wrong repository")
out=$(dispatch_dry "$repo_b" --project "$slug_a" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" "true" "cross-repo: dispatch from B for a story in A succeeds"
assert_eq "$(jqf "$out" .dir)" "$a_phys" "cross-repo: dir is A's checkout, not B"
assert_eq "$(jqf "$out" .worktree_path)" "$a_phys/.claude/worktrees/$id" \
  "cross-repo: the worktree lands in A"

# --- AC1: outside any repository ------------------------------------------

out=$(dispatch_dry "$nowhere" --project "$slug_a" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" "true" "outside: dispatch works from outside any repository"
assert_eq "$(jqf "$out" .dir)" "$a_phys" "outside: dir is A's checkout"

# --- the probe: the git calls themselves run in A --------------------------
#
# Pre-create the branch dispatch would cut, in A and nowhere else. The collision
# guard is a bare `git show-ref`, so under the defect it runs in B, sees nothing,
# and the dispatch proceeds — cutting a SECOND worktree for a story that already
# has one. The refusal is the proof that the guard ran where the worktree is.
probe_id=$(new_story "$repo_a" "Already has a worktree in A")
(cd "$repo_a" && git branch "worktree-$probe_id" >/dev/null 2>&1)
out=$(dispatch_real "$repo_b" --project "$slug_a" dispatch "$probe_id")
assert_eq "$(jqf "$out" .ok)" "false" "probe: the collision guard sees A's branch from B"
assert_contains "$(jqf "$out" .display)" "already exists" "probe: the refusal names the collision"
assert_contains "$(jqf "$out" .display)" "Rolled the claim back" "probe: the claim was rolled back"

# --- AC2: a project with no linked checkout refuses, naming link checkout ---

repo_c=$(mk_story_repo CCC)
slug_c=$(slug_for "$repo_c")
c_id=$(new_story "$repo_c" "Nowhere to run")
(cd "$repo_c" && story project unlink checkout >/dev/null 2>&1)

out=$(dispatch_dry "$repo_b" --project "$slug_c" dispatch "$c_id")
assert_eq "$(jqf "$out" .ok)" "false" "no checkout: dispatch refuses"
assert_contains "$(jqf "$out" .display)" "project link checkout" \
  "no checkout: the refusal names \`project link checkout\`"
assert_contains "$(jqf "$out" .display)" "$slug_c" "no checkout: the refusal names the project"

# The refusal must land BEFORE the compare-and-swap claim, or a dispatch that
# cannot proceed strands its story at in-progress with nothing to show for it.
c_state=$(cd "$repo_c" && story show "$c_id" --json | jq -r '.story.story.state')
assert_eq "$c_state" "todo" "no checkout: the story was never claimed"

# --- a recorded checkout that is not there ---------------------------------

gone=$(mktemp -d /tmp/story-test-gone.XXXXXX)
(cd "$repo_c" && story project link checkout "$gone" >/dev/null 2>&1)
rm -rf "$gone"
out=$(dispatch_dry "$repo_b" --project "$slug_c" dispatch "$c_id")
assert_eq "$(jqf "$out" .ok)" "false" "missing checkout: dispatch refuses"
assert_contains "$(jqf "$out" .display)" "project link checkout" \
  "missing checkout: the refusal names \`project link checkout\`"
assert_contains "$(jqf "$out" .display)" "doctor --fix" \
  "missing checkout: the refusal names what \`doctor --fix\` would cost"

# --- a checkout that is not a git repository -------------------------------
#
# `project link checkout` permits this deliberately (a checkout that has not
# been cloned yet is a legitimate thing to name ahead of time) BECAUSE the one
# consumer fails loudly on its own. This is that failure.

plain=$(mktemp -d /tmp/story-test-plain.XXXXXX)
_TMP_REPOS+=("$plain")
(cd "$repo_c" && story project link checkout "$plain" >/dev/null 2>&1)
out=$(dispatch_dry "$repo_b" --project "$slug_c" dispatch "$c_id")
assert_eq "$(jqf "$out" .ok)" "false" "not a repo: dispatch refuses"
assert_contains "$(jqf "$out" .display)" "not a git repository" \
  "not a repo: the refusal says why"

# --- a checkout that is a LINKED WORKTREE, not a checkout ------------------
#
# Reported, never silently corrected up to the main repo: operating on a
# directory other than the one recorded is the shape of defect this story exists
# to remove, and doing it quietly would only move it.

(cd "$repo_a" && git worktree add -q --no-track -b linked-probe "$repo_a/.claude/worktrees/linked-probe" HEAD) >/dev/null 2>&1
(cd "$repo_c" && story project link checkout "$repo_a/.claude/worktrees/linked-probe" >/dev/null 2>&1)
out=$(dispatch_dry "$repo_b" --project "$slug_c" dispatch "$c_id")
assert_eq "$(jqf "$out" .ok)" "false" "linked worktree: dispatch refuses"
assert_contains "$(jqf "$out" .display)" "linked git worktree" \
  "linked worktree: the refusal says what the directory is"

# --- capture takes the path but neither the cd nor the repo check ----------
#
# capture makes zero bare git calls and its header documents a deliberate
# tolerance for a store that cannot answer, so it must reach its own "no live
# tmux window" report from outside any repository rather than dying on a
# git-repository check it never needed.

out=$(cd "$nowhere" && TMUX="fake,0,0" TMUX_PANE="%0" PATH="$FAKE_TMUX_DIR:$PATH" \
  bash "$SCRIPT" --project "$slug_a" capture "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "capture: no window is still a refusal"
assert_contains "$(jqf "$out" .display)" "no live tmux window" \
  "capture: outside a repository it reports the missing window, not a missing repo"

# --- complete moves with dispatch ------------------------------------------
#
# A worktree dispatched under one rule and completed under the other is SH-166
# verbatim, so all three verbs take their directory the same way.

wt_id=$(new_story "$repo_a" "Completed from the wrong repository")
mk_dispatched "$repo_a" "$wt_id" >/dev/null
out=$(cd "$repo_b" && bash "$SCRIPT" --project "$slug_a" complete plan "$wt_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "complete plan: succeeds from B"
assert_eq "$(jqf "$out" .plan.worktree.path)" "$a_phys/.claude/worktrees/$wt_id" \
  "complete plan: the worktree it plans against is A's"
assert_eq "$(jqf "$out" .plan.worktree.status)" "removable" \
  "complete plan: it can see A's worktree from B"

out=$(cd "$repo_b" && bash "$SCRIPT" --project "$slug_a" complete execute "$wt_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "complete execute: succeeds from B"
assert_eq "$(jqf "$out" '.removed.worktrees[0]')" "$a_phys/.claude/worktrees/$wt_id" \
  "complete execute: it removed A's worktree"
[ -e "$repo_a/.claude/worktrees/$wt_id" ] \
  && fail_test "complete execute: A's worktree is still on disk"
[ -e "$repo_b/.claude/worktrees/$wt_id" ] \
  && fail_test "complete execute: it created something in B"

# --- the caller's own toplevel still decides "current" ---------------------
#
# `_story_worktree_status` asks a CWD-relative question on purpose — "is the
# caller standing inside the worktree it is asking us to delete" — and one real
# `cd` into the checkout destroys the answer unless the caller's toplevel is
# captured before it. Without that, `git worktree remove` succeeds while the
# user's shell sits in a deleted directory.

cur_id=$(new_story "$repo_a" "Completed from inside its own worktree")
cur_wt=$(mk_dispatched "$repo_a" "$cur_id")
out=$(cd "$repo_a/.claude/worktrees/$cur_wt" && bash "$SCRIPT" complete plan "$cur_id" 2>&1)
assert_eq "$(jqf "$out" .plan.worktree.status)" "current" \
  "current: a worktree the caller is standing in is never removable"

# --- no worktree at the current checkout is REPORTED, not reconstructed ----
#
# There is deliberately no CWD fallback for a worktree dispatched before the
# checkout was linked: it only helps when the caller happens to still be
# standing in the old directory, and this codebase reports a disagreement
# between recorded state and reality rather than silently reconstructing it.

orphan_id=$(new_story "$repo_a" "Dispatched somewhere else entirely")
out=$(cd "$repo_b" && bash "$SCRIPT" --project "$slug_a" complete plan "$orphan_id" 2>&1)
assert_eq "$(jqf "$out" .plan.worktree.status)" "missing" "orphan: nothing found in A"
assert_contains "$(jqf "$out" .display)" "link checkout" \
  "orphan: the report names a recent \`link checkout\` as the likely cause"

finish
