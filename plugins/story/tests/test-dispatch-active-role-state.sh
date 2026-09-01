#!/usr/bin/env bash
# SH-481: cmd_dispatch's ID MODE claimed a named story by moving it to the
# LITERAL string `in-progress`, while `is_claimable` (src/domain.rs) excludes a
# story from readiness only when its state equals the project's ACTIVE-ROLE
# state. SH-125 requires `in-progress` to EXIST in every project, but the
# `active` role is assigned separately -- so in a project that moved the role
# onto another slug, dispatch's claim moved the story somewhere that is not the
# active state, `story next` kept handing it out, and the claim did not claim.
#
# `cmd_work` never had this defect: it resolves the target through
# story_active_state, the same resolver `list` uses to exclude an
# already-claimed story from readiness. These cases pin the whole of
# cmd_dispatch onto that resolver.
#
# The fixture puts the active role on `doing` rather than deleting
# `in-progress`: storyhook's own SH-125 invariant forbids removing it, and
# leaving it present is the sharper test anyway -- the buggy target still
# resolves as a valid state, so a claim into it succeeds and reports ok while
# leaving the story ready. Nothing but the readiness question can tell the two
# apart.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# mk_active_role_repo — a fixture project whose `active` role sits on `doing`.
# `in-progress` survives, unroled, exactly as a real project's would.
#
# The role is MOVED rather than duplicated: storyhook refuses two states
# carrying `active` at once, so `in-progress` must give it up first. A fixture
# step that fails aborts the whole file rather than returning an empty path --
# an empty `$repo` would leave every `cd` below standing in this checkout and
# report a project-resolution error in place of the assertion's real verdict.
mk_active_role_repo() {
  local prefix="${1:-ACT}" repo
  repo=$(mk_story_repo "$prefix")
  (
    cd "$repo" || exit 1
    story state add doing --super OPEN >/dev/null || exit 1
    story state set in-progress --role none >/dev/null || exit 1
    story state set doing --role active >/dev/null || exit 1
  ) || return 1
  printf '%s' "$repo"
}

# require_repo <path> <label> — abort the file when a fixture step failed. An
# empty $repo would leave every `cd` below standing in THIS checkout, and each
# assertion would then report storyhook's project-resolution error instead of
# its own verdict.
require_repo() {
  [ -n "$1" ] || { printf 'fixture: %s could not be built\n' "$2" >&2; exit 1; }
}

dispatch_real() {
  local dir="$1"
  shift
  (
    cd "$dir" \
      && PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker \
        bash "$SCRIPT" dispatch "$@" 2>&1
  )
}

# --- fixture premise: the role really did move ------------------------------
# Asserted rather than assumed. If a future storyhook pinned the active role to
# `in-progress`, every case below would pass vacuously against the old bug.
repo=$(mk_active_role_repo)
require_repo "$repo" "the active-role project"
state_list=$(cd "$repo" && story state list)
assert_contains "$state_list" "doing" "premise: the fixture defines a doing state"
active_line=$(printf '%s\n' "$state_list" | grep -E '\(.*active.*\)' || true)
assert_contains "$active_line" "doing" "premise: the active role sits on doing"
case "$active_line" in
  in-progress*) fail_test "premise: the active role is still on in-progress" ;;
esac
printf '%s\n' "$state_list" | grep -q '^in-progress' \
  || fail_test "premise: in-progress still exists in the vocabulary (SH-125)"

# --- the claim lands in the active state, and the story stops being ready ----
id=$(new_story "$repo" "Claim into the active-role state")
out=$(dispatch_real "$repo" "$id")
assert_eq "$(jqf "$out" .ok)" "true" "claim: ok:true"
assert_eq "$(jqf "$out" .state)" "doing" "claim: reported state is the active-role state"
claimed_state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$claimed_state" "doing" "claim: the store agrees the story is at doing"

# The defect's actual damage: a claim that does not claim leaves the story on
# the work-allocation path, so a second claimant is handed work already taken.
next_id=$(cd "$repo" && story next --json | jq -r '.story.story.id // ""')
[ "$next_id" = "$id" ] \
  && fail_test "claim: story next still hands out the claimed story"
ready_ids=$(cd "$repo" && story list --ready --json | jq -r '[.stories[]?.story.id] | join(",")')
case ",$ready_ids," in
  *",$id,"*) fail_test "claim: the claimed story is still in list --ready" ;;
esac

# --- the dry-run preview no longer names a target at all --------------------
# SH-482 collapsed this claim onto `story claim <id>`, which resolves the
# active-role state inside its own write transaction. So the preview names the
# VERB and no state, and this leg's assertion inverts: a state slug appearing
# here again would mean the script had re-acquired the second opinion that was
# SH-481's defect. Where the target actually lands is asserted for real above,
# against the store, which is the stronger question anyway.
dry_id=$(new_story "$repo" "Dry-run preview names the verb, not a target")
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$dry_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry-run: ok:true"
assert_eq "$(jqf "$out" '.commands[0]')" "story claim $dry_id --no-comment" \
  "dry-run: previewed claim command is the verb"
case "$(jqf "$out" '.commands[0]')" in
  *in-progress* | *doing* | *--if-state*)
    fail_test "dry-run: previewed claim command names a client-resolved target state" ;;
esac
dry_state=$(cd "$repo" && story show "$dry_id" --json | jq -r '.story.story.state')
assert_eq "$dry_state" "todo" "dry-run: no claim was actually written"

# --- the already-claimed guard reads the active-role state ------------------
# A story sitting at `doing` is claimed. Redispatch must refuse with the
# specific already-claimed message, not fall through to the ready gate.
guard_repo=$(mk_active_role_repo GRD)
require_repo "$guard_repo" "the guard project"
guard_id=$(new_story "$guard_repo" "Already claimed at doing")
(cd "$guard_repo" && story move "$guard_id" doing >/dev/null)
out=$(cd "$guard_repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$guard_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "guard: ok:false for an already-claimed story"
assert_contains "$(jqf "$out" .display)" "already" "guard: names the story as already claimed"
assert_contains "$(jqf "$out" .display)" "doing" "guard: names the active-role state it saw"
assert_contains "$(jqf "$out" .display)" "--force" "guard: offers the force remedy"

# --- --force reuses an active-role claim without a second transition --------
out=$(cd "$guard_repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$guard_id" --force 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "force: ok:true reusing an active-role claim"
assert_eq "$(jqf "$out" .reused_claim)" "true" "force: reused_claim:true"
assert_eq "$(jqf "$out" '.commands | map(select(startswith("story claim"))) | length')" "0" \
  "force: no redundant claim transition is planned"
assert_contains "$(jqf "$out" .display)" "doing" "force: the reuse note names the active-role state"

# --- --resume also recognizes a customized active-role claim ----------------
# No git/tmux resources survive in this dry-run fixture. Resume permission
# still reuses the claim and adds recovery context without inventing a state
# transition or assuming the active slug is literally `in-progress`.
out=$(cd "$guard_repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$guard_id" --resume 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "resume: ok:true reusing an active-role claim"
assert_eq "$(jqf "$out" .resume_requested)" "true" "resume: permission reported"
assert_eq "$(jqf "$out" .resumed)" "true" "resume: active claim counts as recovered context"
assert_eq "$(jqf "$out" .reused_claim)" "true" "resume: reused_claim:true"
assert_eq "$(jqf "$out" '.commands | map(select(startswith("story claim"))) | length')" "0" \
  "resume: no redundant claim transition is planned"
assert_contains "$(jqf "$out" .prompt)" "resuming work already started" \
  "resume: replacement agent receives recovery context"

# --- the claim rollback releases from the active-role state -----------------
# A dispatch that fails AFTER the claim rolls the story back with an --if-state
# guard. Guarded against the wrong state, the rollback silently does nothing
# and the story is stranded claimed with no worktree. Provoked with target
# session creation failure, which fails after the claim and fresh worktree
# without introducing a recoverable pre-existing resource.
rb_repo=$(mk_active_role_repo RBK)
require_repo "$rb_repo" "the rollback project"
rb_id=$(new_story "$rb_repo" "Rollback from the active-role state")
export STORY_TARGET_SESSION=missing-room STORY_CREATE_SESSION=1 FAKE_TMUX_FAIL_NEW_SESSION=1
out=$(dispatch_real "$rb_repo" "$rb_id")
unset STORY_TARGET_SESSION STORY_CREATE_SESSION FAKE_TMUX_FAIL_NEW_SESSION
assert_eq "$(jqf "$out" .ok)" "false" "rollback: ok:false when target-session creation fails"
assert_contains "$(jqf "$out" .display)" "Rolled the claim back" "rollback: reports a successful rollback"
rb_state=$(cd "$rb_repo" && story show "$rb_id" --json | jq -r '.story.story.state')
assert_eq "$rb_state" "todo" "rollback: the story is released, not stranded at doing"

# --- an unroled project is unchanged ----------------------------------------
# The resolver falls back to `in-progress` when no state carries the role,
# which is every project that has not moved it. Pinned so the fix cannot be
# read as making the common case conditional on new configuration.
plain_repo=$(mk_story_repo PLN)
plain_id=$(new_story "$plain_repo" "Default project still claims into in-progress")
out=$(dispatch_real "$plain_repo" "$plain_id")
assert_eq "$(jqf "$out" .ok)" "true" "default: ok:true"
assert_eq "$(jqf "$out" .state)" "in-progress" "default: still claims into in-progress"

finish
