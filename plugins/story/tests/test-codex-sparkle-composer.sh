#!/usr/bin/env bash
# SH-694: Codex 0.154.0 animates the idle composer of an Astra model with a
# Braille "sparkle" — dots before AND after the placeholder, redrawn every
# 150ms for the whole idle period:
#
#   ›⠁Ask Codex to do anything   ⠈       ⠂  ⠁     ⠄    ⠂         ⠐
#
# input_state reads that row to confirm the initialization turn's submission
# cleared the box (send_prompt_confirmed Phase B, lib/session.sh). Against
# EMPTY_INPUT_PATTERN the decorated row is "text", the 8 x 0.3s confirm window
# expires, and a primer that WAS submitted and stopped by the SessionStart
# hook is reported as bootstrap-submit-unconfirmed. Every autonomous Codex
# dispatch refused that way from 2026-09-10 until this fix.
#
# Two layers, both pinned. The managed launch turns the animation off
# (tui.animations=false; test-dispatch-launch-template.sh pins the flag), and
# input_box_text strips the Braille block before matching, so a launch override
# that keeps animations on, or a decoration nothing anticipated, cannot turn a
# confirmed submission into a refusal silently again. This file pins the
# second layer: the reader against the exact recorded row, and a whole
# dispatch against FAKE_TMUX_CODEX_SPARKLE=1, which makes the fake render the
# field rows and rotate them per capture so no two frames are byte-identical.
#
# MUTATION CHECK (manual, recorded on SH-694): with strip_composer_decoration's
# call removed from input_box_text, the dispatch below refuses with
# wait_ready_reason=bootstrap-submit-unconfirmed and bootstrap_phase=submitted.
source "$(dirname "$0")/lib.sh"

# ---- the reader, in-process, against the recorded row -----------------------
source "$PLUGIN_ROOT/lib/session.sh"
READY_PROMPT_GLYPH='›'
EMPTY_INPUT_PATTERN='^[[:space:]]*Ask Codex to do anything[[:space:]]*$'

sparkled_idle=$(printf '\342\200\272\342\240\201Ask Codex to do anything   \342\240\210       \342\240\202  \342\240\201     \342\240\204    \342\240\202         \342\240\220')
plain_idle=$(printf '\342\200\272 Ask Codex to do anything')
draft=$(printf '\342\200\272 Ask Codex to do anything about SH-1')
sparkled_draft=$(printf '\342\200\272\342\240\201Ask Codex to do anything about SH-1 \342\240\220')
footer='  Plan mode (shift+tab to cycle)'
# A capture is the input row with the footer BELOW it, as the real TUI and the
# fake both render; input_box_text must pick the row, never the footer.
frame() { printf '%s\n%s\n' "$1" "$footer"; }
trim() { sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'; }

# The daemon's locale is whatever it inherited; the strip must not care.
for locale in C en_US.UTF-8; do
  assert_eq "$(LC_ALL=$locale input_box_text "$(frame "$sparkled_idle")" | trim)" \
    "Ask Codex to do anything" "$locale: the sparkled placeholder reads as the plain placeholder"
  assert_eq "$(LC_ALL=$locale input_box_text "$(frame "$plain_idle")" | trim)" \
    "Ask Codex to do anything" "$locale: the plain placeholder is unchanged"
  assert_eq "$(LC_ALL=$locale input_box_text "$(frame "$draft")" | trim)" \
    "Ask Codex to do anything about SH-1" "$locale: a draft keeps its text"
  assert_eq "$(LC_ALL=$locale input_box_text "$(frame "$sparkled_draft")" | trim)" \
    "Ask Codex to do anything about SH-1" "$locale: a decorated draft keeps its text"
done

# Exactly the Braille block goes: its first and last code points (U+2800,
# U+28FF) are removed, while the neighbours on either side (U+27FF, U+2900),
# the box border (U+2502) and the prompt glyph (U+203A) survive.
edge=$(printf 'a\342\237\277b\342\240\200c\342\243\277d\342\244\200e\342\224\202f\342\200\272g')
kept=$(printf 'a\342\237\277bcd\342\244\200e\342\224\202f\342\200\272g')
assert_eq "$(strip_composer_decoration "$edge")" "$kept" \
  "exactly U+2800..U+28FF is removed; neighbours, border and glyph survive"
assert_eq "$(strip_composer_decoration "")" "" "empty input stays empty"

# input_state itself, with tmux answered by a function: the field state was a
# capture that could not be made to match, not a capture that failed.
CAPTURE_FILE=$(mktemp /tmp/story-test-sparkle-capture.XXXXXX)
_TMP_REPOS+=("$CAPTURE_FILE")
tmux() { [ "${1:-}" = capture-pane ] || return 1; cat "$CAPTURE_FILE"; }
frame "$sparkled_idle" > "$CAPTURE_FILE"
assert_eq "$(input_state %1)" empty "a sparkled idle composer is empty"
frame "$plain_idle" > "$CAPTURE_FILE"
assert_eq "$(input_state %1)" empty "a plain idle composer is empty"
frame "$draft" > "$CAPTURE_FILE"
assert_eq "$(input_state %1)" text "a draft containing the placeholder words is text"
frame "$sparkled_draft" > "$CAPTURE_FILE"
assert_eq "$(input_state %1)" text "a decorated draft is text"
unset -f tmux

# ---- the field reproduction: a whole --auto dispatch under a sparkled pane --
FAKE_BIN=$(mktemp -d /tmp/story-test-sparkle-bin.XXXXXX)
_TMP_REPOS+=("$FAKE_BIN")
printf '#!/bin/sh\nexit 0\n' >"$FAKE_BIN/codex"
chmod +x "$FAKE_BIN/codex"
export FAKE_TMUX_STATE
FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-sparkle-tmux.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")
repo=$(mk_story_repo CSP)
id=$(new_story "$repo" "Sparkled composer")
out=$(cd "$repo" && PATH="$FAKE_BIN:$TESTS_DIR/fakes:$PATH" \
  TMUX=fake TMUX_PANE=%0 STORY_AGENT=codex \
  STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=2 STORY_CONFIRM_DELAY=0 \
  STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
  FAKE_TMUX_CODEX_SPARKLE=1 \
  FAKE_TMUX_CODEX_SENTINEL_MODE=identity \
  FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" \
  bash "$SCRIPT" dispatch "$id" --auto)
assert_eq "$(jqf "$out" .ok)" true \
  "a sparkled composer no longer defeats the submission check (wait_ready_reason=$(jqf "$out" .wait_ready_reason) bootstrap_phase=$(jqf "$out" .bootstrap_phase))"
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits")" 2 "one initialization turn and one charter"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" "$id" "the charter reached the pane"
frames=$(cat "$FAKE_TMUX_STATE/sparkle_frame" 2>/dev/null || printf 0)
[ "$frames" -gt 1 ] || fail_test "the fake rendered $frames sparkle frame(s); the animation must have been observed more than once"
finish
