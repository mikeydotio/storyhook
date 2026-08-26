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
# The claim is SILENT (`--no-comment`), exactly as SH-477 wired NEXT MODE, and
# for the same mechanical reason: `story claim`'s default sentence names the
# CALLING process's tmux window, which inside dispatch is the DISPATCHER's
# window, not the window dispatch is about to open for the work. SH-490 owns
# the question of what dispatch should say once that window exists; this file
# pins that it says nothing AT THE CLAIM, so a future edit that drops
# `--no-comment` is caught here rather than discovered as a wrong sentence in
# the tracker.
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

# --- and it is silent ------------------------------------------------------
# The claim writes a state transition and NOTHING else. A comment here would
# name the dispatcher's own tmux window as the place the work is happening,
# which is the one thing SH-476's determination forbids outright.
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
  "control: the comment filter can see a comment at all -- otherwise the silence assertion below is vacuous"

comment_count=$(printf '%s' "$log" | jq -r '[.log[] | select(.kind == "StoryCommentAdded")] | length')
assert_eq "$comment_count" "0" "claim: the dispatch claim posts no comment (--no-comment, SH-490)"

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
assert_eq "$(jqf "$out" '.commands[0]')" "story claim $dry_id --no-comment" \
  "dry-run: the previewed claim command is the verb"
assert_contains "$(jqf "$out" .display)" "would claim it via \`story claim $dry_id --no-comment\`" \
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

# And the unforced guard still refuses BEFORE the verb, with its own more
# specific message -- not with the verb's `conflict`, whose `expected` is the
# pseudo-state `unclaimed` and reads as a lost race rather than a redispatch.
out=$(cd "$force_repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$force_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "guard: an already-claimed story is still refused"
assert_contains "$(jqf "$out" .display)" "already" "guard: names the story as already claimed"
assert_contains "$(jqf "$out" .display)" "--force" "guard: offers the force remedy"

finish
