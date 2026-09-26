# composer.awk — the text that stands in a provider's composer.
#
# Input: a tmux capture of an agent pane, from `capture-pane -p -e` so that
# character attributes arrive as SGR sequences. A plain capture reads the same,
# minus the attribute rule below.
# Environment: COMPOSER_GLYPH, the provider's prompt glyph (required).
# Output: the text of the ACTIVE composer row -- the last line that bears the
# glyph -- after that line's first glyph, with every escape sequence removed and
# every character drawn faint (SGR 2) left out.
# Exit status: 0 when some line bears the glyph, 1 when none does. Any other
# status is a failure of this program and answers nothing (input_state, in
# lib/session.sh, reads it as "unknown").
#
# Faint text is left out because providers draw what is NOT input that way:
# Claude Code's predicted next prompt in an empty composer, Codex's "Ask Codex
# to do anything" placeholder (both recorded on 2026-09-26, SH-780). Typed or
# pasted input is never faint. tmux carries SGR state from one captured line to
# the next, so the whole capture is interpreted from its first byte.
#
# Every doubt resolves to text. storyhook types into a composer only when it
# reads empty, so an unknown or unterminated escape sequence is kept as text,
# never dropped.
#
# Run it with LC_ALL=C: the glyph is matched as bytes and no locale is needed.

BEGIN {
    esc = sprintf("%c", 27)
    bel = sprintf("%c", 7)
    glyph = ENVIRON["COMPOSER_GLYPH"]
    faint = 0
    found = 0
    answer = ""
    if (glyph == "") {
        failed = 1
        exit 2
    }
}

{
    rest = $0
    line_text = ""
    seen = 0
    while (rest != "") {
        at = index(rest, esc)
        if (at == 0) {
            visible(rest)
            break
        }
        if (at > 1)
            visible(substr(rest, 1, at - 1))
        rest = substr(rest, at)
        used = sequence(rest)
        if (used == 0) {
            # Unknown or unterminated: the rest of the line stays text.
            visible(rest)
            break
        }
        rest = substr(rest, used + 1)
    }
    if (seen) {
        found = 1
        answer = line_text
    }
}

END {
    if (failed)
        exit 2
    printf "%s", answer
    exit (found ? 0 : 1)
}

# visible(s) — s is drawn text in the current attributes. Before this line's
# first glyph it is only searched for the glyph (whatever its attributes);
# after it, it is kept unless it is faint.
function visible(s,    at) {
    if (!seen) {
        at = index(s, glyph)
        if (at == 0)
            return
        seen = 1
        s = substr(s, at + length(glyph))
    }
    if (!faint)
        line_text = line_text s
}

# sequence(s) — s starts with ESC. Apply it and return how many bytes it uses,
# or 0 when it is not a complete CSI or OSC sequence.
function sequence(s,    n, i, c, body, stop, st) {
    n = length(s)
    if (n < 2)
        return 0
    c = substr(s, 2, 1)
    if (c == "[") {
        # CSI: parameter and intermediate bytes, then one final byte 0x40-0x7E.
        for (i = 3; i <= n; i++) {
            c = substr(s, i, 1)
            if (c >= "@" && c <= "~") {
                if (c == "m")
                    sgr(substr(s, 3, i - 3))
                return i
            }
        }
        return 0
    }
    if (c == "]") {
        # OSC (an OSC 8 hyperlink, say): up to BEL or ESC backslash.
        body = substr(s, 3)
        stop = index(body, bel)
        st = index(body, esc "\\")
        if (stop > 0 && (st == 0 || stop < st))
            return 2 + stop
        if (st > 0)
            return 2 + st + 1
        return 0
    }
    return 0
}

# sgr(p) — the parameters of one SGR sequence. Only faint matters here: an empty
# parameter or 0 resets, 2 sets it, 22 clears it. The arguments of an extended
# colour (38, 48, 58 followed by 5;n or 2;r;g;b) are skipped, because they are
# numbers, not attributes. A parameter with colon sub-parameters (tmux writes
# 4:2 for a double underline, 58:2::r:g:b for an underline colour) is not a
# plain number, so it is never read as faint.
function sgr(p,    n, part, i, code) {
    if (p == "") {
        faint = 0
        return
    }
    n = split(p, part, ";")
    for (i = 1; i <= n; i++) {
        code = part[i]
        if (code !~ /^[0-9]*$/)
            continue
        if (code + 0 == 0)
            faint = 0
        else if (code + 0 == 2)
            faint = 1
        else if (code + 0 == 22)
            faint = 0
        else if (code + 0 == 38 || code + 0 == 48 || code + 0 == 58) {
            if (part[i + 1] == "5")
                i += 2
            else if (part[i + 1] == "2")
                i += 4
        }
    }
}
