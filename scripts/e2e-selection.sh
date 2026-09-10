#!/usr/bin/env bash
#
# The browser harness's "does this project select anything?" question, answered
# in a way that cannot mistake a failed question for an empty answer (SH-625).
#
# `scripts/run-e2e.sh` runs one Playwright invocation per project and asks
# `--list` first, so a filter that only matches, say, `.mobile.spec.ts$` files
# skips `chromium`/`webkit` instead of aborting the whole loop over a project
# the caller never meant to filter into (SH-335). Until SH-625 that probe
# discarded both stderr and the exit status and read the answer as text, and
# Playwright's text is the SAME for an empty selection and for a listing that
# never happened -- so a spec that failed to load, an unknown flag, or a config
# that would not evaluate was reported as "selects no tests -- skipping", and
# the run exited 0. SH-306's shape inside the browser harness: a check that
# did not run must never read as a pass.
#
# MEASURED on the pinned Playwright below (`e2e/package.json`), this
# repository's own config, no browser, no daemon, under bash -- re-run this
# matrix and update e2e_selection_measured_playwright when the pin moves
# (`tests/e2e_selection.rs` fails the build until both agree):
#
#   --list --reporter=list --pass-with-no-tests     stdout                        exit
#   filter matches nothing / nonexistent file / -g  Total: 0 tests in 0 files     0
#   one selected spec fails to load                 Total: 0 tests in 0 files     1 (+ the error on stderr)
#   unknown flag / unknown project                  (no Total: line)              1
#   four spec files under chromium                  Total: 34 tests in 4 files    0
#
# Without `--pass-with-no-tests` the first two rows are byte-identical on
# stdout AND both exit 1 (`runner/index.js`: the `passWithNoTests` guard is
# the only thing between "no tests" and a thrown error). With it, exit 0
# means "here is the selection, possibly empty" and nonzero means "the
# invocation failed" -- the two facts this file exists to keep apart, told
# apart by Playwright's own exit status rather than by matching the wording
# of an error message.
#
# Deliberately no top-level `set` here: this file is sourced by a caller that
# has already chosen its own shell options (gate-progress.sh's own rule).
# Every function below checks each step explicitly instead, because `set -e`
# is suppressed inside a `x="$(fn)" || ...` assignment anyway (SH-578).

# Prints the Playwright version the matrix above was measured against.
# Compared to `e2e/package.json`'s exact pin by `tests/e2e_selection.rs`, so
# an upgrade has to re-ask the question by name: the drift this file cannot
# detect at run time is a future Playwright that exits 0 on a load error,
# which would quietly reopen SH-625.
e2e_selection_measured_playwright() {
    printf '%s\n' "1.63.0"
}

# Return codes of `e2e_list_selection`, named so a caller branches on a word.
# A failed invocation returns 1 and names Playwright's own status in its
# message, so this namespace never collides with a child's exit code.
E2E_SELECTION_EMPTY=3
E2E_SELECTION_UNREADABLE=4

# Prints the test count from a `--reporter=list` listing's own summary line
# (`Total: N tests in M files`; `test`/`file` singular for 1). Returns 1 and
# prints nothing when the listing carries no such line. Portable BRE, not
# `\+`/`\?`: macOS's BSD sed supports neither GNU extension, and run-e2e.sh's
# shebang resolves to it.
e2e_selection_total() {
    local total
    total="$(printf '%s\n' "$1" | sed -n 's/^Total: \([0-9][0-9]*\) tests\{0,1\}.*/\1/p' | head -n1)"
    [ -n "$total" ] || return 1
    printf '%s\n' "$total"
}

# Runs `CMD... --list --reporter=list --pass-with-no-tests` with stderr sent
# to STDERR_FILE, and turns the outcome into one of four verdicts:
#
#   0                          at least one test selected; the listing is on stdout
#   $E2E_SELECTION_EMPTY       the invocation succeeded and selected nothing
#   $E2E_SELECTION_UNREADABLE  it succeeded but printed no `Total:` line -- the
#                              output shape drifted, and a count this file
#                              cannot read is not a count of zero
#   1                          the invocation itself failed; STDERR_FILE is
#                              replayed to stderr so the cause (the spec that
#                              would not load, the flag Playwright rejected)
#                              is named rather than dropped
#
# Nothing reaches stdout on any verdict but 0, so a caller that captures the
# listing can never mistake a refusal message for one.
e2e_list_selection() {
    local stderr_file listing status total
    stderr_file="$1"
    shift
    status=0
    listing="$("$@" --list --reporter=list --pass-with-no-tests 2>"$stderr_file")" || status=$?
    if [ "$status" -ne 0 ]; then
        cat "$stderr_file" >&2
        printf 'e2e-selection: the listing itself failed (exit %s): %s\n' "$status" "$*" >&2
        printf '  refusing, not skipping -- a listing that did not happen is not an empty selection (SH-625)\n' >&2
        return 1
    fi
    total="$(e2e_selection_total "$listing")" || {
        printf 'e2e-selection: the listing exited 0 but printed no "Total: N tests" line: %s\n' "$*" >&2
        printf '  Playwright'"'"'s --reporter=list summary shape has changed; refusing rather than reading an unreadable count as zero (SH-625)\n' >&2
        return "$E2E_SELECTION_UNREADABLE"
    }
    if [ "$total" -eq 0 ]; then
        return "$E2E_SELECTION_EMPTY"
    fi
    printf '%s\n' "$listing"
}
