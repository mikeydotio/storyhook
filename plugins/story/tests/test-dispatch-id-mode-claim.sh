#!/usr/bin/env bash
# SH-482: cmd_dispatch's ID MODE claims through `story claim <id>`, the one
# atomic claim verb, instead of hand-rolling the same compare-and-swap out of
# `story show` + `story move --if-state`.
#
# Both dispatch modes now reach the store through ONE primitive. What that is
# worth is not fewer lines -- it is that the active-role target, the CAS and
# the `claimed_from` answer are all resolved inside the CLI's own write
# transaction, so there is no second opinion in this script to drift from the
# CLI's (SH-481 was that drift; `claim_rollback_note`'s own doc named this
# story as what closes the last of it).
#
# SH-490 settles the comment asymmetry: ID MODE knows the future window name,
# so its claim and truthful intent comment are one transaction. NEXT MODE does
# not know its id yet and comments only after the resources exist.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

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

# --- the claim really is the verb, read back out of the store ---------------
# Asserted against `story log`'s own `command` field rather than this script's
# output: what matters is what got WRITTEN, not what story.sh believed it sent
# (test-dispatch-actor-labels.sh's rule, applied to the claim half).
repo=$(mk_story_repo)
id=$(new_story "$repo" "ID-MODE claim routes through the verb")

out=$(dispatch_real "$repo" "$id")
assert_eq "$(jqf "$out" .ok)" "true" "claim: ok:true"
assert_eq "$(jqf "$out" .claimed)" "true" "claim: claimed:true"
assert_eq "$(jqf "$out" .state)" "in-progress" "claim: reported state is the claimed state"

log=$(cd "$repo" && story log "$id" --json)
claim_command=$(printf '%s' "$log" \
  | jq -r '[.log[] | select(.actor == "story.sh:dispatch") | .command] | unique | join(",")')
assert_eq "$claim_command" "claim" \
  "claim: the dispatch claim is recorded as \`claim\`, not a hand-rolled set-state"

# --- and its intent comment is part of that claim --------------------------
# The future window name is deterministic from the caller-supplied id. The
# session must come from tmux, never from the dispatcher's own window name.
#
# POSITIVE CONTROL FIRST, and it is not decoration: written without one, this
# assertion named the event kind `StoryCommented`, which storyhook does not
# emit -- so it counted zero of a kind that can never appear and passed with
# `--no-comment` deleted. A filter nothing can match is not evidence of
# silence (SH-364: the gate was right; the fixture lied to it). The control is
# taken from a SEPARATE story so the subject's own log stays untouched.
control_id=$(new_story "$repo" "Positive control for the comment filter")
(cd "$repo" && story comment "$control_id" 'a comment the filter must be able to see' >/dev/null)
control_log=$(cd "$repo" && story log "$control_id" --json)
control_count=$(printf '%s' "$control_log" | jq -r '[.log[] | select(.kind == "StoryCommentAdded")] | length')
assert_eq "$control_count" "1" \
  "control: the comment filter can see a comment at all -- otherwise the exact assertion below is vacuous"

comment_entries=$(printf '%s' "$log" \
  | jq -c '[.log[] | select(.kind == "StoryCommentAdded") | {actor,command,detail}]')
assert_eq "$comment_entries" \
  "[{\"actor\":\"story.sh:dispatch\",\"command\":\"claim\",\"detail\":\"comment — Dispatching to tmux window story-session:$id.\"}]" \
  "claim: the transactional comment names the future target window"

# --- the dry-run preview names the verb, and no client-resolved state -------
# SH-481's own dry-run leg used to assert the previewed command named the
# active-role state, because the script picked that target itself and could
# pick it wrong. It no longer picks one at all: the command it previews is the
# command it runs, and the target lives inside the CLI's write transaction.
# The preview must therefore name NO state slug -- naming one would be this
# script re-acquiring the second opinion the collapse exists to delete.
dry_id=$(new_story "$repo" "Dry-run previews the verb")
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$dry_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry-run: ok:true"
assert_eq "$(jqf "$out" '.commands[0]')" \
  "story claim $dry_id --comment \"Dispatching to tmux window <current-session>:$dry_id.\"" \
  "dry-run: the previewed claim command is the verb"
assert_contains "$(jqf "$out" .display)" \
  "would claim it via \`story claim $dry_id --comment \"Dispatching to tmux window <current-session>:$dry_id.\"\`" \
  "dry-run: display names the claim command it would run"
case "$(jqf "$out" '.commands[0]')" in
  *in-progress* | *--if-state*)
    fail_test "dry-run: the preview still carries a client-resolved claim target" ;;
esac
dry_state=$(cd "$repo" && story show "$dry_id" --json | jq -r '.story.story.state')
assert_eq "$dry_state" "todo" "dry-run: no claim was actually written"

# --- --force still reuses a claim without reaching the verb ----------------
# The already-claimed guard is a PREDICATE, and `story claim` cannot serve as
# one: answering "is this already claimed?" through the verb means attempting
# the claim. So the guard keeps its own `story_active_state` read, and the
# forced path must still plan no claim command at all.
force_repo=$(mk_story_repo FRC)
force_id=$(new_story "$force_repo" "Forced reuse plans no claim")
(cd "$force_repo" && story move "$force_id" in-progress >/dev/null)
out=$(cd "$force_repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$force_id" --force 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "force: ok:true reusing the existing claim"
assert_eq "$(jqf "$out" .reused_claim)" "true" "force: reused_claim:true"
assert_eq "$(jqf "$out" '.commands | map(select(startswith("story claim"))) | length')" "0" \
  "force: no claim command is planned for a reused claim"
assert_eq "$(jqf "$out" '.commands | map(select(startswith("story comment"))) | length')" "1" \
  "force: the dry-run plans the post-handoff resource record"

# And the unforced guard still refuses BEFORE the verb, with its own more
# specific message -- not with the verb's `conflict`, whose `expected` is the
# pseudo-state `unclaimed` and reads as a lost race rather than a redispatch.
out=$(cd "$force_repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$force_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "guard: an already-claimed story is still refused"
assert_contains "$(jqf "$out" .display)" "already" "guard: names the story as already claimed"
assert_contains "$(jqf "$out" .display)" "--force" "guard: offers the force remedy"

forced=$(dispatch_real "$force_repo" "$force_id" --force)
assert_eq "$(jqf "$forced" .ok)" "true" "force: real redispatch succeeds"
force_repo_real=$(cd "$force_repo" && pwd -P)
force_comment=$(cd "$force_repo" && story show "$force_id" --json \
  | jq -r '.story.story.comments[-1].text')
assert_eq "$force_comment" \
  "Dispatched to tmux window story-session:$force_id, worktree $force_repo_real/.claude/worktrees/$force_id, branch worktree-$force_id." \
  "force: a successful claim-reuse records its resources after handoff"

# A real ID claim needs a real target session. If tmux cannot resolve one,
# dispatch refuses before the claim rather than fabricating a location.
session_repo=$(mk_story_repo SES)
session_id=$(new_story "$session_repo" "No tmux session identity")
export FAKE_TMUX_NO_SESSION=1
session_out=$(dispatch_real "$session_repo" "$session_id")
unset FAKE_TMUX_NO_SESSION
assert_eq "$(jqf "$session_out" .ok)" "false" "session: missing identity refuses"
assert_contains "$(jqf "$session_out" .display)" "no claim was made" \
  "session: refusal names the no-side-effect boundary"
assert_eq "$(cd "$session_repo" && story show "$session_id" --json | jq -r '.story.story.state')" \
  "todo" "session: unresolved target leaves the story ready"
[ ! -d "$session_repo/.claude/worktrees/$session_id" ] \
  || fail_test "session: unresolved target created a worktree"

finish
