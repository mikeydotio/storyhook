#!/usr/bin/env bash
# `story.sh unclaim <id>` (SH-484) — the plugin half of `story unclaim`: the
# store release (SH-483), then the story's tmux window. NOTHING on disk is
# touched, which is the whole line between this verb and its destructive
# sibling `reset`, and several assertions below exist only to hold that line.
#
# The self-window case is the one worth reading the design comment on SH-484
# for: unclaim does everything it was asked to do and SKIPS ONLY the window
# kill, because killing the caller's own pane destroys the very answer
# SH-483's determination requires be said out loud (which state the story went
# back to, and whether that was a fallback). It never refuses on this — `reset`
# does, because `reset` cannot skip its destructive step.
source "$(dirname "$0")/lib.sh"

export PATH="$TESTS_DIR/fakes:$PATH"
export FAKE_TMUX_STATE
FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")

repo=$(mk_story_repo)
slug=$(slug_for "$repo")

# The fake tmux creates its log lazily, so an assertion that no window was
# closed cannot read a file that may never have existed.
kill_count() { wc -l <"$FAKE_TMUX_STATE/kill_window_args.log" 2>/dev/null || printf '0'; }

state_of() { (cd "$repo" && story show "$1" --json | jq -r '.story.story.state'); }
comments_of() { (cd "$repo" && story show "$1" --json | jq -r '[.story.story.comments[].text] | join("|")'); }
claim_it() { (cd "$repo" && story claim "$1" --no-comment --json >/dev/null 2>&1); }

# --- happy path: releases, and closes the window that is NOT the caller's ---
hp=$(new_story "$repo" "Hand me back")
claim_it "$hp"
out=$(cd "$repo" \
  && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="$(printf '%s\t1\t%%7' "$hp")" \
     bash "$SCRIPT" --project "$slug" unclaim "$hp" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "happy: ok"
assert_eq "$(jqf "$out" .unclaimed_from)" "in-progress" "happy: names the state released"
assert_eq "$(jqf "$out" .restored_to)" "todo" "happy: names where it landed"
assert_eq "$(jqf "$out" .window)" "open" "happy: the window was someone else's"
assert_eq "$(jqf "$out" .closed_window)" "true" "happy: and it was closed"
assert_eq "$(state_of "$hp")" "todo" "happy: the REAL story really moved"
grep -q -- '-t @1' "$FAKE_TMUX_STATE/kill_window_args.log" \
  || fail_test "happy: tmux kill-window did not target the resolved window"

# --- nothing on disk is touched, and the survivor is REPORTED ---------------
# A dirty worktree is left exactly as found: unclaim's entire promise. It is
# also named in the answer, because an operator who wanted the worktree gone
# needs to be told which verb does that rather than left to discover it.
wt=$(new_story "$repo" "Worktree survives")
wwt=$(mk_dispatched "$repo" "$wt")
echo scratch >"$repo/.claude/worktrees/$wwt/scratch.txt"
claim_it "$wt"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$wt" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "worktree: ok"
assert_eq "$(jqf "$out" .worktree_status)" "dirty" "worktree: its state is reported, not hidden"
[ -d "$repo/.claude/worktrees/$wwt" ] || fail_test "worktree: unclaim removed a worktree"
[ -f "$repo/.claude/worktrees/$wwt/scratch.txt" ] || fail_test "worktree: uncommitted work was destroyed"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$wwt") \
  || fail_test "worktree: unclaim deleted a branch"
assert_contains "$(jqf "$out" .display)" "reset" "worktree: display points at the verb that would remove it"

# --- a story nobody claimed is a REFUSAL, and the window survives it --------
nc=$(new_story "$repo" "Never claimed")
out=$(cd "$repo" \
  && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="$(printf '%s\t1\t%%7' "$nc")" \
     bash "$SCRIPT" --project "$slug" unclaim "$nc" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unclaimed: ok:false"
assert_eq "$(jqf "$out" .reason)" "unclaim-conflict" "unclaimed: reason is unclaim-conflict"
assert_contains "$(jqf "$out" .display)" "todo" "unclaimed: names the state actually found"
assert_eq "$(state_of "$nc")" "todo" "unclaimed: nothing moved"
kills_before=$(kill_count)
out=$(cd "$repo" \
  && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="$(printf '%s\t1\t%%7' "$nc")" \
     bash "$SCRIPT" --project "$slug" unclaim "$nc" 2>&1)
assert_eq "$(kill_count)" "$kills_before" \
  "unclaimed: a lost CAS never closes somebody's window"

# --- SELF-TERMINATION: the release happens, the window kill does not --------
sf=$(new_story "$repo" "Unclaimed from its own window")
claim_it "$sf"
kills_before=$(kill_count)
out=$(cd "$repo" \
  && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="$(printf '%s\t1\t%%0' "$sf")" \
     bash "$SCRIPT" --project "$slug" unclaim "$sf" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "self: still ok — the release is the verb"
assert_eq "$(jqf "$out" .window)" "self" "self: the window is classified as the caller's own"
assert_eq "$(jqf "$out" .closed_window)" "false" "self: it was NOT closed"
assert_eq "$(state_of "$sf")" "todo" "self: the release really happened"
assert_contains "$(jqf "$out" .display)" "left open" "self: the skip is named, never silent"
assert_eq "$(kill_count)" "$kills_before" \
  "self: no kill-window call was made at all"

# --- no window anywhere is not an error ------------------------------------
nw=$(new_story "$repo" "No window")
claim_it "$nw"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$nw" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "no-window: ok"
assert_eq "$(jqf "$out" .window)" "none" "no-window: reported as none"
assert_eq "$(jqf "$out" .closed_window)" "false" "no-window: nothing was closed"

# --- a window the CALLER cannot see is still closed -------------------------
# `_complete_prepare` classifies the window only when the caller is itself
# inside tmux, which answers "is this mine" but not "does one exist". From a
# shell outside the server a live story window used to classify as `none` and
# be left open in silence. `unclaim` now asks the closer itself and reports
# what it actually found -- and a caller with no pane of their own structurally
# cannot be standing in the window being closed.
ow=$(new_story "$repo" "Window I cannot see")
claim_it "$ow"
kills_before=$(kill_count)
out=$(cd "$repo" \
  && FAKE_TMUX_PANES="$(printf '%s\t1\t%%7' "$ow")" \
     bash "$SCRIPT" --project "$slug" unclaim "$ow" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "invisible-window: ok"
assert_eq "$(jqf "$out" .window)" "open" "invisible-window: found and reported as open"
assert_eq "$(jqf "$out" .closed_window)" "true" "invisible-window: and actually closed"
[ "$(kill_count)" -gt "$kills_before" ] || fail_test "invisible-window: no kill-window call was made"

# --- the comment flags reach the CLI verbatim -------------------------------
cm=$(new_story "$repo" "With a reason")
claim_it "$cm"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$cm" --comment "blocked on TST-9" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "comment: ok"
assert_contains "$(comments_of "$cm")" "blocked on TST-9" "comment: the caller's own sentence is stored"

nq=$(new_story "$repo" "Quietly")
claim_it "$nq"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$nq" --no-comment 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "no-comment: ok"
assert_eq "$(comments_of "$nq")" "" "no-comment: nothing was stored"

out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$nq" --comment x --no-comment 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "comment+no-comment: refused rather than resolved by precedence"

# --- the fallback is reported, not performed silently -----------------------
# A story created directly in the active state has no earlier state to go back
# to; SH-483 requires that be said out loud rather than substituted quietly.
fb=$(cd "$repo" && story new "Born claimed" --state in-progress --json | jq -r '.story.story.id')
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$fb" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "fallback: ok"
assert_eq "$(jqf "$out" .restore_fallback)" "no-prior-state" "fallback: the reason is carried through"
assert_eq "$(jqf "$out" .restored_to)" "todo" "fallback: landed on the required fallback state"
assert_contains "$(jqf "$out" .display)" "no-prior-state" "fallback: and display says so"

# --- dry run: reads for real, writes nothing --------------------------------
dr=$(new_story "$repo" "Dry run me")
claim_it "$dr"
kills_before=$(kill_count)
out=$(cd "$repo" \
  && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="$(printf '%s\t1\t%%7' "$dr")" \
     STORY_DRY_RUN=1 bash "$SCRIPT" --project "$slug" unclaim "$dr" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry: ok"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry: flagged"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "story unclaim" "dry: previews the release"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "kill-window" "dry: previews the window close"
assert_eq "$(cd "$repo" \
  && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="$(printf '%s\t1\t%%0' "$dr")" \
     STORY_DRY_RUN=1 bash "$SCRIPT" --project "$slug" unclaim "$dr" 2>&1 \
  | jq -r '.commands|join(" ")' | grep -c 'kill-window' || true)" "0" \
  "dry: a preview from the story's own window does NOT promise to close it"
assert_eq "$(state_of "$dr")" "in-progress" "dry: the story did NOT move"
assert_eq "$(kill_count)" "$kills_before" \
  "dry: no window was closed"

# A dry run that would conflict says so, rather than previewing a release that
# could not happen: the CLI's own --dry-run reads for real.
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" --project "$slug" unclaim "$nc" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "dry-conflict: a preview of an impossible release refuses"
assert_eq "$(jqf "$out" .reason)" "unclaim-conflict" "dry-conflict: for the same reason a real run would"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" unclaim 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unclaim: missing id is ok:false"
assert_contains "$(jqf "$out" .display)" "usage:" "unclaim: missing id shows the usage line"
out=$(cd "$repo" && bash "$SCRIPT" unclaim "bad id!" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unclaim: invalid id is ok:false"
assert_contains "$(jqf "$out" .display)" "alphanumeric" "unclaim: invalid id names the constraint"
out=$(cd "$repo" && bash "$SCRIPT" --project "$slug" unclaim "$nc" --force 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unclaim: --force is not a flag this verb has"

finish
