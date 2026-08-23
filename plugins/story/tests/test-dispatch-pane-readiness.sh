#!/usr/bin/env bash
# SH-178 repro (RCA investigation dispatch-types-the-agent-charter, GRID at
# .rca/dispatch-types-the-agent-charter/): a pane that never became a Claude
# TUI still receives and submits the dispatch charter.
#
# FIXED by SH-226: wait_ready now requires the pane's foreground command to match
# READY_PROCESS_PATTERN -- a process-table fact a shell prompt cannot fake --
# and story.sh step 12 REFUSES instead of typing when it is not satisfied.
#
# THE MECHANISM (see lib/session.sh wait_ready, story.sh steps 11-13). wait_ready's
# Tier 2 "structural" fallback is satisfied by ANY pane whose capture holds a
# frame rule (READY_FRAME_GLYPH, default '─') and an idle prompt glyph
# (READY_PROMPT_GLYPH, default '❯'), stable across READY_STABLE_POLLS
# consecutive captures -- it never checks that Claude is what's running there.
# In the field, the pane's shell was still starting when story.sh pasted the
# launch command; oh-my-zsh's interactive update prompt ate the leading `c`,
# so the shell saw `laude ...` -> "zsh: command not found: laude" and Claude
# never started. The pane settled into a bare, stable P10k shell prompt --
# which supplies both glyphs -- so Tier 2 confirmed "ready" in ~0.75s. story.sh
# step 13 then typed and submitted the full agent charter into that shell.
# Every backticked span in the charter (`story show`, `/council-vote`,
# `make test`, `gh pr merge --merge`, `story move <n> done`, `story block <n>
# "<reason>"`) is a zsh command substitution to a bare shell, so the story
# reached `done` with no work, no commit, no PR, no comment.
#
# FAKE_TMUX_CAPTURE=structural (plugins/story/tests/fakes/tmux) models
# exactly that pane: a static frame rule + idle glyph under a footer that
# matches none of READY_PATTERN's known Claude markers ("for shortcuts",
# "mode on", "to cycle", ...) -- i.e. never a real Claude TUI, just something
# that happens to render the same two glyphs a shell prompt theme supplies.
#
# SUPERSEDED MECHANISM, SAME REGRESSION COVERAGE (SH-231). Dispatch no longer
# calls wait_ready (the tiers described above) at all -- it polls
# wait_ready_sentinel for a published dispatch sentinel instead, immune to
# rendered content by construction. FAKE_TMUX_CAPTURE=structural is now inert
# for dispatch specifically (nothing reads pane content on this path any
# more) and survives here only because FAKE_TMUX_LAUNCH_MANGLE=1 is what
# actually drives this scenario: no live claude/node process, so no
# SessionStart hook, so no sentinel -- this file keeps pinning "the SH-178
# pane never receives the charter," it just does so via wait_ready_sentinel's
# own no-sentinel timeout rather than a process-name mismatch on rendered
# content. wait_ready itself is unchanged and still governs cmd_doctor's
# scratch-window self-test, which this file does not exercise.
#
# THE FAILING ASSERTION: story.sh must never submit the charter into a pane
# that was never confirmed as an actual Claude session. The fake tmux's
# private $FAKE_TMUX_STATE/prompt_submits and .../submitted are the ground
# truth for what actually got typed and sent -- independent of what story.sh
# CLAIMS in its own JSON result -- and today they show the charter landing
# and submitting anyway.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

export FAKE_TMUX_STATE
FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
_TMP_REPOS+=("$FAKE_TMUX_STATE")
unset FAKE_TMUX_SESSIONS FAKE_TMUX_FAIL_NEW_SESSION FAKE_TMUX_DROP_PASTE FAKE_TMUX_ENTER_ABSORB

repo=$(mk_story_repo)
id=$(new_story "$repo" "Ready story dispatched into a pane that never started Claude")

out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=structural FAKE_TMUX_LAUNCH_MANGLE=1 \
      bash "$SCRIPT" dispatch "$id" --auto 2>&1
)

# --- The gate: readiness must be REFUSED, and the refusal must carry the
#     evidence an operator needs. Before SH-226 this asserted `true` -- it was
#     the sanity pin proving the structural tier fast-passed on a bare shell.
assert_eq "$(jqf "$out" .readiness_confirmed)" "false" \
  "structural-idle-pane: readiness must NOT be confirmed on a pane whose occupant is a shell"
assert_eq "$(jqf "$out" .reason)" "pane-not-ready" \
  "structural-idle-pane: the refusal names the pane, not some other guard"
# SH-231: readiness moved from a rendered-content check to sentinel
# existence, so a pane that never actually started claude/node -- exactly
# this scenario, the SH-178 field cause -- now reads "no-sentinel" (nothing
# ever published one) rather than "wrong-process" (a real sentinel exists but
# this pane's occupant doesn't match it). The pane's occupant is still
# reported below regardless of which reason fires -- an operator
# investigating a timeout still needs to know what is actually sitting there.
assert_eq "$(jqf "$out" .wait_ready_reason)" "no-sentinel" \
  "structural-idle-pane: the refusal says WHY -- a shell that never started claude, not a timeout"
assert_eq "$(jqf "$out" .pane_command)" "zsh" \
  "structural-idle-pane: the observed occupant is reported, so an operator can set STORY_READY_PROCESS_PATTERN"

# --- THE DEFECT: the charter must not have been typed into this pane. ------
prompt_submits="$(cat "$FAKE_TMUX_STATE/prompt_submits" 2>/dev/null || echo 0)"
assert_eq "$prompt_submits" "0" \
  "structural-idle-pane: the agent charter must never submit into a pane that was never confirmed as Claude"

submitted="$(cat "$FAKE_TMUX_STATE/submitted" 2>/dev/null || echo '')"
case "$submitted" in
  *"story move $id done"*)
    fail_test "structural-idle-pane: the charter's own \`story move $id done\` landed in a bare shell prompt -- the exact SH-178 field failure (zsh would command-substitute it, silently closing the story with no work done)"
    ;;
esac

# --- Secondary: dispatch should report failure and leave the story
#     unclaimed rather than a false-positive success. ---
assert_eq "$(jqf "$out" .ok)" "false" \
  "structural-idle-pane: dispatch must not report success when the pane was never confirmed as an actual Claude session"
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "todo" \
  "structural-idle-pane: the story must not be claimed by a dispatch that never actually reached Claude"

finish
