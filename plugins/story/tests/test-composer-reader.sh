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
ARGS_FILE=$(mktemp /tmp/story-test-composer-args.XXXXXX)
_TMP_REPOS+=("$CAPTURE_FILE" "$ARGS_FILE")
tmux() {
  [ "${1:-}" = capture-pane ] || return 1
  printf '%s\n' "$*" >"$ARGS_FILE"
  cat "$CAPTURE_FILE"
}

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

# ---- faint text is not input (F2, F3) ----------------------------------------
# Claude draws a predicted next prompt faint (SGR 2) in the empty composer, and
# Codex draws its placeholder faint. Both read as "text" to a plain capture,
# which made an idle agent look busy and made a dropped paste look received.
# The reader asks tmux for attributes (capture-pane -e) and skips faint text.
# tmux carries SGR state from one line to the next (the composer row starts
# with \033[39m to undo the rule above it), so the whole capture is read.
ghost_row='\033[39m\342\235\257\302\240\033[2mmove SH-769 back to verifying\033[0m\n'
claude_ghost="\033[37m${claude_rule}${ghost_row}\033[37m${claude_rule}\033[39m${claude_footer}"
# pane %48, a Claude selection dialog: the cursor is the glyph and an ASCII space.
dialog_row='\033[94m\342\235\257\033[39m \033[37m1.\033[39m \033[94mOpen resume picker (Recommended)\033[39m\n'
codex_bg='\033[48;2;109;104;133m'
codex_idle="\033[1m\342\200\272\033[0m${codex_bg} \033[2mAsk Codex to do anything\033[0m${codex_bg}\n  ? for shortcuts\n"
codex_draft="\033[1m\342\200\272\033[0m${codex_bg} fix SH-1\033[0m${codex_bg}\n  ? for shortcuts\n"
row() { printf '%s' "${claude_rule}\342\235\257\302\240$1\n${claude_rule}${claude_footer}"; }

for locale in C en_US.UTF-8; do
  assert_eq "$(state "$locale" '❯' "$claude_ghost")" empty \
    "$locale: Claude's faint prompt suggestion is an idle composer"
  assert_contains " $(cat "$ARGS_FILE") " " -e " "$locale: the capture asks tmux for attributes"
  assert_eq "$(state "$locale" '❯' "$(row 'draft for SH-1')")" text "$locale: a drawn draft is text"
  assert_eq "$(state "$locale" '❯' "${claude_rule}${dialog_row}\033[37m  2. New session\033[39m\n")" text \
    "$locale: a dialog's cursor row is text"
  assert_eq "$(state "$locale" '›' "$codex_idle")" empty \
    "$locale: Codex's faint placeholder is empty with no EMPTY_INPUT_PATTERN at all"
  assert_eq "$(state "$locale" '›' "$codex_draft")" text \
    "$locale: a Codex draft on a truecolour background is text"
  # The digits of an extended colour are arguments, not attributes.
  assert_eq "$(state "$locale" '❯' "$(row '\033[48;2;2;2;2mdraft')")" text "$locale: 48;2;r;g;b is not faint"
  assert_eq "$(state "$locale" '❯' "$(row '\033[38;5;2mdraft')")" text "$locale: 38;5;n is not faint"
  assert_eq "$(state "$locale" '❯' "$(row '\033[58;2;9;2;2mdraft')")" text "$locale: 58;2;r;g;b is not faint"
  # tmux writes underline styles as colon sub-parameters: 4:2 is a double underline.
  assert_eq "$(state "$locale" '❯' "$(row '\033[4:2mdraft')")" text "$locale: 4:2 is an underline, not faint"
  assert_eq "$(state "$locale" '❯' "$(row '\033[2m\033[58:2::1:2:3mghost\033[0m')")" empty \
    "$locale: a colon underline colour neither sets nor clears faint"
  assert_eq "$(state "$locale" '❯' "$(row '\033[0;2mghost\033[0m')")" empty "$locale: 0;2 sets faint"
  assert_eq "$(state "$locale" '❯' "$(row '\033[1;2mghost\033[0m')")" empty "$locale: 1;2 sets faint"
  assert_eq "$(state "$locale" '❯' "$(row '\033[2mghost\033[22mdraft')")" text "$locale: 22 ends faint"
  assert_eq "$(state "$locale" '❯' "$(row '\033[2mghost\033[mdraft')")" text "$locale: an empty SGR resets"
  assert_eq "$(state "$locale" '❯' "$(row '\033[2mghost \033[38;5;246mstill ghost\033[39m')")" empty \
    "$locale: a colour change keeps faint"
  assert_eq "$(state "$locale" '❯' "\033[2m  a faint hint\n\342\235\257\302\240ghost\n")" empty \
    "$locale: faint set on an earlier line still holds"
  # OSC 8 hyperlinks (Codex draws them) are not text; their label is.
  assert_eq "$(state "$locale" '❯' "$(row '\033]8;;https://example.invalid\033\\link\033]8;;\033\\')")" text \
    "$locale: a hyperlink's label is text"
  assert_eq "$(state "$locale" '❯' "$(row '\033[2m\033]8;;https://example.invalid\007ghost\033]8;;\007\033[0m')")" empty \
    "$locale: a faint hyperlink is not"
  # Every doubt resolves to text: "empty" lets notify type, "text" refuses.
  assert_eq "$(state "$locale" '❯' "$(row '\033[2')")" text "$locale: an unterminated CSI is text"
  assert_eq "$(state "$locale" '❯' "$(row '\033]8;;never-ends')")" text "$locale: an unterminated OSC is text"
  assert_eq "$(state "$locale" '❯' "$(row '\033(0q')")" text "$locale: an unknown escape is text"
  # The glyph marks the row whatever its attributes.
  assert_eq "$(state "$locale" '❯' "\033[2m\342\235\257\302\240\033[0mdraft\n")" text \
    "$locale: a faint glyph still marks the row"
  assert_eq "$(state "$locale" '❯' "\342\235\257\302\240old draft\n\033[2m\342\235\257\302\240ghost\033[0m\n")" empty \
    "$locale: the last row with a glyph is the composer, even when that glyph is faint"
done

# ---- strict: no composer row is not an idle composer --------------------------
# Plain input_state keeps reading "no row" as "empty" (a submitted prompt may
# leave none). Before storyhook TYPES, it asks the strict question: a screen
# with no composer row (a full-screen view, a mode with another prompt) is
# "absent", and nothing is typed into it.
strict_state() {
  printf "$3" >"$CAPTURE_FILE"
  (export LC_ALL="$1"; READY_PROMPT_GLYPH="$2"; EMPTY_INPUT_PATTERN=''; input_state %1 strict)
}
for locale in C en_US.UTF-8; do
  assert_eq "$(strict_state "$locale" '❯' "  a transcript view
  (no prompt)
")" absent \
    "$locale: strict: no composer row is absent"
  assert_eq "$(state "$locale" '❯' "  a transcript view
  (no prompt)
")" empty \
    "$locale: plain: no composer row still reads empty"
  assert_eq "$(strict_state "$locale" '❯' "$claude_ghost")" empty "$locale: strict: an idle composer is empty"
  assert_eq "$(strict_state "$locale" '❯' "$(row 'draft')")" text "$locale: strict: a draft is text"
done

# ---- composer_holds: the composer shows THIS prompt ---------------------------
# "Any text" was the receipt, and a dialog's cursor row is text too. The
# signature is the prompt's first line (at most 20 bytes), or the placeholder
# the provider puts in the input when it collapses a long paste (Claude:
# "[Pasted text #N +M lines]", Codex: "[Pasted Content N chars]", both read from
# the 2026-09 binaries). Faint text counts here: a placeholder's styling is not
# the question, whether the input holds our paste is.
holds() {  # holds <locale> <glyph> <placeholder pattern> <capture> <text>
  printf "$4" >"$CAPTURE_FILE"
  (export LC_ALL="$1"; READY_PROMPT_GLYPH="$2"; PASTE_PLACEHOLDER_PATTERN="$3"
   if composer_holds %1 "$5"; then printf holds; else printf no; fi)
}
remediation=$(printf 'CENTRAL VERIFICATION RED: the gate failed.\nFix the existing PR.')
claude_ph="$CLAUDE_PASTE_PLACEHOLDER_PATTERN"
codex_ph="$CODEX_PASTE_PLACEHOLDER_PATTERN"
for locale in C en_US.UTF-8; do
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row 'CENTRAL VERIFICATION RED: the gate failed.')" "$remediation")" holds \
    "$locale: the prompt's first line is on the row"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row 'CENTRAL VERIFICATION RED: the gate f')" "$remediation")" holds \
    "$locale: a row that wraps after the signature still holds"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row '[Pasted text #1 +12 lines]')" "$remediation")" holds \
    "$locale: Claude's collapsed paste"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row '[Pasted text #3]')" "$remediation")" holds \
    "$locale: Claude's collapsed single-line paste"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row '\033[2m[Pasted text #1 +3 lines]\033[0m')" "$remediation")" holds \
    "$locale: a faint placeholder is still our paste"
  assert_eq "$(holds "$locale" '›' "$codex_ph" "\342\200\272 [Pasted Content 1234 chars]\n  ? for shortcuts\n" "$remediation")" holds \
    "$locale: Codex's collapsed paste"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row '  CENTRAL VERIFICATION RED')" "$(printf '  CENTRAL VERIFICATION RED\tx')")" holds \
    "$locale: leading blanks and a tab do not break the signature"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row '\303\234berpr\303\274fung der \303\204nderung')" "$(printf '\303\234berpr\303\274fung der \303\204nderung')")" holds \
    "$locale: a non-ASCII first line holds"
  # Not our paste: nothing may be submitted.
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$claude_ghost" "$remediation")" no "$locale: a ghost suggestion"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "${claude_rule}${dialog_row}" "$remediation")" no "$locale: a dialog row"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row '')" "$remediation")" no "$locale: an empty composer"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "  no composer here\n" "$remediation")" no "$locale: no composer row"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row 'fix the flaky test')" "$remediation")" no "$locale: a person's draft"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row 'x [Pasted text #1 +3 lines]')" "$remediation")" no \
    "$locale: a placeholder after a draft is not our paste alone"
  assert_eq "$(holds "$locale" '›' "$codex_ph" "\342\200\272 [Pasted text #1 +3 lines]\n" "$remediation")" no \
    "$locale: one provider's placeholder is not the other's"
done
assert_eq "$(COMPOSER_AWK=/nonexistent/composer.awk holds C '❯' "$claude_ph" "$(row 'CENTRAL VERIFICATION RED')" "$remediation" 2>/dev/null)" no \
  "a failing reader holds nothing"

# ---- SH-799: the reads dispatch's handoff makes ---------------------------------
# send_prompt_confirmed now types only into a composer that reads idle, takes
# THIS prompt as receipt, and confirms a submission only once the composer
# neither shows the prompt nor holds input.
#
# A fresh Claude session shows 'Try "..."' in its empty composer until the first
# submission, and a dispatch pastes into exactly that session. Claude Code
# 2.1.283 draws the placeholder dim, or with its first letter inverted when it
# paints its own cursor cell. The recorded idle rows above (F1, F2) carry no
# inverted cell, so the field rendering is the all-faint one, and it is idle.
# The inverted variant has a non-faint first letter and reads as text: dispatch
# then refuses rather than type. That failure is closed and visible, and this
# pins it, so a provider change shows up here first.
fresh_row='\033[39m\342\235\257\302\240\033[2mTry "fix lint errors"\033[0m\n'
fresh_inverted_row='\033[39m\342\235\257\302\240\033[7mT\033[27m\033[2mry "fix lint errors"\033[0m\n'
charter='Investigate and plan a fix for story SH-7 in this repo. Begin by reading it.'
for locale in C en_US.UTF-8; do
  assert_eq "$(strict_state "$locale" '❯' "${claude_rule}${fresh_row}${claude_rule}${claude_footer}")" empty \
    "$locale: a fresh session's faint 'Try' placeholder is an idle composer"
  assert_eq "$(strict_state "$locale" '❯' "${claude_rule}${fresh_inverted_row}${claude_rule}${claude_footer}")" text \
    "$locale: an inverted first letter is not faint, so the composer is not idle (fails closed)"
  assert_eq "$(holds "$locale" '❯' "$claude_ph" "$(row 'Investigate and plan a fix for story SH-7 in this re[...Truncated text #1 +0 lines...]')" "$charter")" holds \
    "$locale: Claude's truncated long input still begins with the charter"
  assert_eq "$(holds "$locale" '›' "$codex_ph" "\342\200\272 Storyhook initialization only. Do not use tools,\n  ? for shortcuts\n" \
    'Storyhook initialization only. Do not use tools, ask questions.')" holds \
    "$locale: the Codex initialization turn shows inline"
done

# cleared <locale> <glyph> <placeholder pattern> <capture> <text> — composer_cleared
cleared() {
  printf "$4" >"$CAPTURE_FILE"
  (export LC_ALL="$1"; READY_PROMPT_GLYPH="$2"; PASTE_PLACEHOLDER_PATTERN="$3"; EMPTY_INPUT_PATTERN=''
   if composer_cleared %1 "$5"; then printf cleared; else printf no; fi)
}
for locale in C en_US.UTF-8; do
  assert_eq "$(cleared "$locale" '❯' "$claude_ph" "$(row '')" "$charter")" cleared "$locale: an empty composer is cleared"
  assert_eq "$(cleared "$locale" '❯' "$claude_ph" "  working...\n" "$charter")" cleared \
    "$locale: no composer row is cleared (a submission may leave none)"
  assert_eq "$(cleared "$locale" '❯' "$claude_ph" "$claude_ghost" "$charter")" cleared \
    "$locale: a faint ghost suggestion after the submission is cleared"
  assert_eq "$(cleared "$locale" '❯' "$claude_ph" "$(row '\033[2m[Pasted text #1]\033[0m')" "$charter")" no \
    "$locale: a FAINT placeholder is still the charter, not a cleared composer"
  assert_eq "$(cleared "$locale" '❯' "$claude_ph" "$(row 'Investigate and plan a fix')" "$charter")" no \
    "$locale: the charter on the row is not cleared"
  assert_eq "$(cleared "$locale" '❯' "$claude_ph" "${claude_rule}${dialog_row}" "$charter")" no \
    "$locale: a dialog is not a cleared composer"
done

# idle_polls <capture> — "<status> <last state> <captures>" of poll_composer_idle.
# A composer that is not drawn yet is waited for on the READY budget (Claude's
# readiness never looked at the screen). A drawn row holding text is a dialog
# or a draft, which does not leave by itself, so it is given CONFIRM_ATTEMPTS
# reads and no more: the engine kills a dispatch after 180 s.
COUNT_FILE=$(mktemp /tmp/story-test-composer-count.XXXXXX)
_TMP_REPOS+=("$COUNT_FILE")
idle_polls() {
  printf "$1" >"$CAPTURE_FILE"
  : >"$COUNT_FILE"
  (tmux() { [ "${1:-}" = capture-pane ] || return 1; printf x >>"$COUNT_FILE"; cat "$CAPTURE_FILE"; }
   READY_PROMPT_GLYPH='❯'; EMPTY_INPUT_PATTERN=''; READY_ATTEMPTS=12; READY_DELAY=0; CONFIRM_ATTEMPTS=3
   last=$(poll_composer_idle %1) && status=0 || status=$?
   printf '%s %s %s' "$status" "$last" "$(wc -c <"$COUNT_FILE" | tr -d ' ')")
}
assert_eq "$(idle_polls "$(row '')")" "0 empty 1" "an idle composer is taken at once"
assert_eq "$(idle_polls "${claude_rule}${dialog_row}")" "1 text 3" "a dialog is given CONFIRM_ATTEMPTS reads"
assert_eq "$(idle_polls "  starting...\n")" "1 absent 12" "a composer not drawn yet is waited for on the READY budget"

# A reader that fails has not read an empty composer.
assert_eq "$(COMPOSER_AWK=/nonexistent/composer.awk state C '❯' "$(row '')" 2>/dev/null)" unknown \
  "a failing reader is unknown, never empty"

finish
