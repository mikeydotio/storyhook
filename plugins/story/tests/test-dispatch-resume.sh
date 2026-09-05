#!/usr/bin/env bash
# SH-523: reconstruct an abandoned dispatch from whichever claim, branch,
# worktree and tmux pane survived. Existing git work is evidence to preserve.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

fresh_tmux_state() {
  FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
  export FAKE_TMUX_STATE
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
}

dispatch_real() {
  local repo="$1" id="$2"
  shift 2
  (
    cd "$repo" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="${CALLER_PANE:-%0}" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch "$id" "$@" 2>&1
  )
}

# Permission is not an assertion: no surviving resources means a fresh run.
repo_fresh=$(mk_story_repo RFR)
id_fresh=$(new_story "$repo_fresh" "Fresh dispatch with resume permission")
fresh=$(dispatch_real "$repo_fresh" "$id_fresh" --resume)
assert_eq "$(jqf "$fresh" .ok)" "true" "fresh: dispatch succeeds"
assert_eq "$(jqf "$fresh" .resume_requested)" "true" "fresh: permission reported"
assert_eq "$(jqf "$fresh" .resumed)" "false" "fresh: no resume invented"

# A dirty worktree without a window is offered, then resumed in place.
fresh_tmux_state
unset FAKE_TMUX_PANES
repo_wt=$(mk_story_repo RWT)
id_wt=$(new_story "$repo_wt" "Abandoned dirty worktree")
mk_dispatched "$repo_wt" "$id_wt" >/dev/null
printf 'keep me\n' >"$repo_wt/.claude/worktrees/$id_wt/resume-proof.txt"
(cd "$repo_wt" && story move "$id_wt" in-progress >/dev/null)

offered=$(dispatch_real "$repo_wt" "$id_wt")
assert_eq "$(jqf "$offered" .ok)" "false" "offer: ordinary dispatch stops"
assert_eq "$(jqf "$offered" .reason)" "resume-available" "offer: typed reason"
assert_eq "$(jqf "$offered" .resources.worktree)" "present" "offer: worktree inventory"
assert_eq "$(jqf "$offered" .resources.window)" "missing" "offer: window inventory"

before_transitions=$(
  (cd "$repo_wt" && story export) \
    | jq --arg id "$id_wt" '[.stories[] | select(.id == $id).events[] | select(.kind == "StoryStateChanged")] | length'
)
resumed_wt=$(dispatch_real "$repo_wt" "$id_wt" --resume)
assert_eq "$(jqf "$resumed_wt" .ok)" "true" "worktree: resume succeeds"
assert_eq "$(jqf "$resumed_wt" .resumed)" "true" "worktree: resume reported"
assert_eq "$(jqf "$resumed_wt" .reused_claim)" "true" "worktree: claim reused"
assert_eq "$(jqf "$resumed_wt" .worktree_reused)" "true" "worktree: tree reused"
assert_eq "$(jqf "$resumed_wt" .window_reused)" "false" "worktree: window created"
assert_eq "$(cat "$repo_wt/.claude/worktrees/$id_wt/resume-proof.txt")" "keep me" \
  "worktree: uncommitted bytes preserved"
after_transitions=$(
  (cd "$repo_wt" && story export) \
    | jq --arg id "$id_wt" '[.stories[] | select(.id == $id).events[] | select(.kind == "StoryStateChanged")] | length'
)
assert_eq "$after_transitions" "$before_transitions" "worktree: no redundant transition"
submitted=$(cat "$FAKE_TMUX_STATE/submitted" 2>/dev/null || printf '')
assert_contains "$submitted" "resuming work already started" "prompt: names resumed work"
assert_contains "$submitted" "previous agent" "prompt: names prior-agent uncertainty"
assert_contains "$submitted" "Implement the approved work" "prompt: ordinary charter remains"
resume_comment=$(cd "$repo_wt" && story show "$id_wt" --json \
  | jq -r '.story.story.comments[-1].text')
repo_wt_real=$(cd "$repo_wt" && pwd -P)
assert_eq "$resume_comment" \
  "Dispatched to tmux window story-session:$id_wt, worktree $repo_wt_real/.claude/worktrees/$id_wt, branch worktree-$id_wt." \
  "worktree: claim-reuse records the resumed resources after handoff"

# A later handoff failure rolls back only this attempt's resources. The dirty
# worktree and branch inherited above remain evidence even though the newly
# launched provider never became ready.
fresh_tmux_state
unset FAKE_TMUX_PANES
repo_preserve=$(mk_story_repo RPR)
id_preserve=$(new_story "$repo_preserve" "Preserve reused work on handoff failure")
mk_dispatched "$repo_preserve" "$id_preserve" >/dev/null
printf 'still mine\n' >"$repo_preserve/.claude/worktrees/$id_preserve/failure-proof.txt"
(cd "$repo_preserve" && story move "$id_preserve" in-progress >/dev/null)
export FAKE_TMUX_LAUNCH_MANGLE=1
failed_resume=$(dispatch_real "$repo_preserve" "$id_preserve" --resume)
unset FAKE_TMUX_LAUNCH_MANGLE
assert_eq "$(jqf "$failed_resume" .ok)" "false" "rollback ownership: readiness refusal"
assert_eq "$(jqf "$failed_resume" .reason)" "pane-not-ready" "rollback ownership: typed reason"
assert_eq "$(cat "$repo_preserve/.claude/worktrees/$id_preserve/failure-proof.txt")" "still mine" \
  "rollback ownership: reused dirty bytes survive"
(cd "$repo_preserve" && git show-ref --verify --quiet "refs/heads/worktree-$id_preserve") \
  || fail_test "rollback ownership: reused branch was deleted"

# A surviving branch without a worktree is reattached at its existing commit.
fresh_tmux_state
unset FAKE_TMUX_PANES
repo_branch=$(mk_story_repo RBR)
id_branch=$(new_story "$repo_branch" "Abandoned branch only")
mk_dispatched "$repo_branch" "$id_branch" >/dev/null
branch_wt="$repo_branch/.claude/worktrees/$id_branch"
printf 'committed work\n' >"$branch_wt/branch-proof.txt"
(cd "$branch_wt" && git add branch-proof.txt && git commit -qm 'fixture work')
branch_oid=$(cd "$repo_branch" && git rev-parse "worktree-$id_branch")
(cd "$repo_branch" && git worktree remove "$branch_wt")
(cd "$repo_branch" && story move "$id_branch" in-progress >/dev/null)

resumed_branch=$(dispatch_real "$repo_branch" "$id_branch" --resume)
assert_eq "$(jqf "$resumed_branch" .ok)" "true" "branch: resume succeeds"
assert_eq "$(jqf "$resumed_branch" .worktree_reused)" "false" "branch: tree reconstructed"
assert_eq "$(jqf "$resumed_branch" .branch_reused)" "true" "branch: existing branch reused"
assert_eq "$(cd "$branch_wt" && git rev-parse HEAD)" "$branch_oid" "branch: commit retained"
assert_eq "$(cat "$branch_wt/branch-proof.txt")" "committed work" "branch: bytes retained"

# A surviving pane is respawned; no competing window is created.
fresh_tmux_state
repo_window=$(mk_story_repo RWN)
id_window=$(new_story "$repo_window" "Abandoned worktree and pane")
mk_dispatched "$repo_window" "$id_window" >/dev/null
(cd "$repo_window" && story move "$id_window" in-progress >/dev/null)
export FAKE_TMUX_PANES="$id_window	1	%9"
resumed_window=$(dispatch_real "$repo_window" "$id_window" --resume)
assert_eq "$(jqf "$resumed_window" .ok)" "true" "window: resume succeeds"
assert_eq "$(jqf "$resumed_window" .window_reused)" "true" "window: pane reused"
repo_window_real=$(cd "$repo_window" && pwd -P)
assert_contains "$(cat "$FAKE_TMUX_STATE/respawn_pane_args.log" 2>/dev/null || printf '')" \
  "-k -c $repo_window_real/.claude/worktrees/$id_window" "window: respawn cwd"
[ ! -f "$FAKE_TMUX_STATE/new_window_args.log" ] \
  || fail_test "window: opened a competing window"

# A pane can outlive both git resources. Resume creates a fresh branch and
# worktree, then reuses that exact pane instead of opening a competitor.
fresh_tmux_state
repo_pane_only=$(mk_story_repo RPO)
id_pane_only=$(new_story "$repo_pane_only" "Abandoned pane only")
(cd "$repo_pane_only" && story move "$id_pane_only" in-progress >/dev/null)
export FAKE_TMUX_PANES="$id_pane_only	1	%8"
resumed_pane_only=$(dispatch_real "$repo_pane_only" "$id_pane_only" --resume)
assert_eq "$(jqf "$resumed_pane_only" .ok)" "true" "pane only: resume succeeds"
assert_eq "$(jqf "$resumed_pane_only" .worktree_created)" "true" \
  "pane only: missing worktree created"
assert_eq "$(jqf "$resumed_pane_only" .branch_created)" "true" \
  "pane only: missing branch created"
assert_eq "$(jqf "$resumed_pane_only" .window_reused)" "true" \
  "pane only: surviving pane reused"

# Unsafe identities and self-replacement refuse without damaging evidence.
fresh_tmux_state
unset FAKE_TMUX_PANES
repo_unsafe=$(mk_story_repo RUN)
id_unsafe=$(new_story "$repo_unsafe" "Unregistered path collision")
mkdir -p "$repo_unsafe/.claude/worktrees/$id_unsafe"
printf 'not yours\n' >"$repo_unsafe/.claude/worktrees/$id_unsafe/evidence.txt"
(cd "$repo_unsafe" && story move "$id_unsafe" in-progress >/dev/null)
unsafe=$(dispatch_real "$repo_unsafe" "$id_unsafe" --resume)
assert_eq "$(jqf "$unsafe" .ok)" "false" "unsafe: unregistered path refuses"
assert_eq "$(jqf "$unsafe" .reason)" "resume-unsafe" "unsafe: typed reason"
assert_eq "$(cat "$repo_unsafe/.claude/worktrees/$id_unsafe/evidence.txt")" "not yours" \
  "unsafe: evidence preserved"

fresh_tmux_state
repo_wrong=$(mk_story_repo RWB)
id_wrong=$(new_story "$repo_wrong" "Expected path on the wrong branch")
(cd "$repo_wrong" && git worktree add -q -b "other-$id_wrong" \
  ".claude/worktrees/$id_wrong" HEAD)
(cd "$repo_wrong" && story move "$id_wrong" in-progress >/dev/null)
wrong=$(dispatch_real "$repo_wrong" "$id_wrong" --resume)
assert_eq "$(jqf "$wrong" .reason)" "resume-unsafe" "wrong branch: typed refusal"
assert_contains "$(jqf "$wrong" .display)" "not \`worktree-$id_wrong\`" \
  "wrong branch: both identities reported"
[ -d "$repo_wrong/.claude/worktrees/$id_wrong" ] \
  || fail_test "wrong branch: existing worktree was removed"

fresh_tmux_state
repo_protected=$(mk_story_repo RPB)
id_protected=$(new_story "$repo_protected" "Protected expected branch")
mk_dispatched "$repo_protected" "$id_protected" >/dev/null
(cd "$repo_protected" && story move "$id_protected" in-progress >/dev/null)
export STORY_PROTECTED_BRANCHES="worktree-$id_protected"
protected=$(dispatch_real "$repo_protected" "$id_protected" --resume)
unset STORY_PROTECTED_BRANCHES
assert_eq "$(jqf "$protected" .reason)" "resume-unsafe" "protected branch: typed refusal"
assert_contains "$(jqf "$protected" .display)" "protected" "protected branch: policy reported"
[ -d "$repo_protected/.claude/worktrees/$id_protected" ] \
  || fail_test "protected branch: existing worktree was removed"

fresh_tmux_state
repo_self=$(mk_story_repo RSL)
id_self=$(new_story "$repo_self" "Current pane cannot replace itself")
mk_dispatched "$repo_self" "$id_self" >/dev/null
(cd "$repo_self" && story move "$id_self" in-progress >/dev/null)
export FAKE_TMUX_PANES="$id_self	1	%0"
self=$(dispatch_real "$repo_self" "$id_self" --resume)
assert_eq "$(jqf "$self" .ok)" "false" "self: refuses"
assert_eq "$(jqf "$self" .reason)" "resume-unsafe" "self: typed reason"
assert_contains "$(jqf "$self" .display)" "current pane" "self: diagnosis"

finish
