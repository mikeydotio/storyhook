#!/usr/bin/env bash
# SH-780: the composer reader (input_box_text / input_state, lib/session.sh)
# against rows recorded from live panes on 2026-09-26 with
# `tmux capture-pane -p -e` (Claude Code 2.1.283, Codex 0.157.0).
#
# story.sh notify types a resume or a remediation into an agent's pane only
# when input_state reads the composer as idle, so every way this reader can be
# wrong is a way to type into a dialog or to refuse an idle agent. The rows
# below are the field bytes, not reconstructions.
source "$(dirname "$0")/lib.sh"
source "$PLUGIN_ROOT/lib/session.sh"

CAPTURE_FILE=$(mktemp /tmp/story-test-composer-capture.XXXXXX)
_TMP_REPOS+=("$CAPTURE_FILE")
tmux() { [ "${1:-}" = capture-pane ] || return 1; cat "$CAPTURE_FILE"; }

# state <locale> <glyph> <printf-format capture> — input_state of that capture
# under that locale. The locale is exported in a subshell: the reader's own
# pattern matching runs in the caller's shell, and a temporary assignment in
# front of a function call is not a reliable way to switch bash's locale.
state() {
  printf "$3" >"$CAPTURE_FILE"
  (export LC_ALL="$1"; READY_PROMPT_GLYPH="$2"; EMPTY_INPUT_PATTERN=''; input_state %1)
}

claude_rule='\342\224\200\342\224\200\342\224\200\342\224\200\n'
claude_footer='  \342\217\265\342\217\265 auto mode on (shift+tab to cycle)\n'

# ---- NBSP padding (F1) -------------------------------------------------------
# Claude pads its prompt glyph with U+00A0, not a space. [:space:] does not
# match those two bytes in the C locale, and the daemon's locale is whatever
# started it (the launchd plist sets none), so an idle composer read as "text"
# there and every guarded delivery would refuse an idle agent.
for locale in C en_US.UTF-8; do
  assert_eq "$(state "$locale" '❯' "${claude_rule}\342\235\257\302\240\n${claude_rule}${claude_footer}")" \
    empty "$locale: Claude's idle composer (glyph + NBSP) is empty"
  assert_eq "$(state "$locale" '❯' "${claude_rule}\342\235\257\302\240draft for SH-1\n${claude_rule}${claude_footer}")" \
    text "$locale: a draft after the NBSP is text"
  assert_eq "$(state "$locale" '❯' "${claude_rule}\342\235\257\302\240\302\240\302\240\n${claude_rule}${claude_footer}")" \
    empty "$locale: NBSP alone is padding, however much of it"
done

# ---- the composer's text starts at its FIRST glyph ---------------------------
# The row's own prompt glyph comes first. A draft that ends in the glyph
# character (a person quoting the prompt, say) is still a draft; reading from
# the LAST glyph made it an empty composer.
for locale in C en_US.UTF-8; do
  assert_eq "$(state "$locale" '❯' "${claude_rule}\342\235\257\302\240the prompt looks like \342\235\257\n${claude_rule}${claude_footer}")" \
    text "$locale: a draft that ends in the glyph is text"
  assert_eq "$(state "$locale" '›' "\342\200\272 quote \342\200\272\n  ? for shortcuts\n")" \
    text "$locale: the same for Codex's glyph"
done

finish
