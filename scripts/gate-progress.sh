#!/usr/bin/env bash
#
# The SH-524 gate progress journal -- one append-only NDJSON line per
# emission, so the daemon-owned centralized verifier can read live release-
# gate progress from a subprocess it deliberately does not hand STORYHOOK_
# STORE_PATH to (SH-521 sanitized that subprocess on purpose; this file must
# not undo that by giving it another way to reach the store).
#
# CONTRACT: every emitter in this repository -- scripts/leg.sh,
# scripts/run-tests.sh, plugins/story/tests/run-tests.sh, scripts/run-e2e.sh,
# scripts/verify-pr.sh -- is a no-op when $STORYHOOK_GATE_PROGRESS is unset,
# so interactive `make test` is byte-identical to before this file existed.
# Never gate that no-op on anything else (a `-t 1` TTY check, an environment
# guess): the daemon is the only caller that sets the variable, and its
# absence is what makes an interactive run inert.
#
# Two line shapes, both objects with a "kind" field:
#
#   {"kind":"item","path":"release gate/fmt","status":"passed","at":"...",
#    "seconds":2}
#     One checklist row, addressed by a "/"-joined label path from the root.
#     Later "item" lines for the same path OVERWRITE the prior one when the
#     journal is folded (SH-524's Rust-side fold is last-write-wins per
#     path) -- so a leg's "running" line followed by its "passed" line is
#     the ordinary shape, not a special case.
#   {"kind":"case","path":"release gate/rust-suite","outcome":"pass"}
#     One test case finishing under the suite item at `path`. Carries no
#     timestamp on purpose -- cases can arrive in the thousands, and none of
#     the checklist's promises (a suite's live pass/fail counts) need
#     per-case wall-clock resolution. Journal *file* mtime is what the
#     Rust-side publisher reads for staleness, not any embedded timestamp.
#
# Status vocabulary an "item" line's "status" may hold:
#   pending | running | passed | failed | skipped | reused
#
# json_escape and emit_item are meant to be sourced (`. gate-progress.sh`)
# by a caller that emits more than a couple of lines, so it pays one date(1)
# fork per emission rather than one bash fork too. A caller emitting only a
# handful of top-level phase/leg boundaries may invoke this file directly:
#   bash scripts/gate-progress.sh item "release gate/fmt" running
#
# Deliberately no top-level `set` here: this file is sourced by callers that
# have already chosen their own shell options (leg.sh relies on `-e`), and a
# `set` at source time would silently rewrite the *caller's* flags -- bash
# applies `set` in the shell that runs it, sourced or not. Only the direct-
# invocation entrypoint at the bottom sets its own.

# Escapes `s` for embedding in a JSON string: backslash and double-quote,
# per RFC 8259 -- the two characters that would otherwise terminate or
# corrupt the string. Every field this file writes is either a fixed status
# word, a caller-chosen label, or a numeral; none of them need control-
# character escaping in practice, so this covers exactly what could occur.
gate_progress_json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# The journal path this run should write to, or empty if progress reporting
# is off. A single accessor so every emitter agrees on the one environment
# variable that turns this on.
gate_progress_journal() {
    printf '%s' "${STORYHOOK_GATE_PROGRESS:-}"
}

# Appends one "item" line. Extra key=value pairs after `status` are appended
# as additional JSON fields verbatim (the caller is trusted to pass valid
# JSON fragments, e.g. `seconds=12` or `total=1451 estimated=true`) -- kept
# to numbers and bare words on purpose; a string value needs its own quoting
# by the caller, and nothing here does that today.
#
# No-op, silently, when $STORYHOOK_GATE_PROGRESS is unset -- the contract
# every caller of this file relies on.
gate_progress_emit_item() {
    local journal path status
    journal="$(gate_progress_journal)"
    [ -n "$journal" ] || return 0
    path="$1"
    status="$2"
    shift 2
    local extra=""
    for kv in "$@"; do
        extra="$extra,\"${kv%%=*}\":${kv#*=}"
    done
    printf '{"kind":"item","path":"%s","status":"%s","at":"%s"%s}\n' \
        "$(gate_progress_json_escape "$path")" \
        "$status" \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$extra" \
        >>"$journal"
}

# Appends one "case" line: one finished test case under the suite item at
# `path`. No-op, silently, when $STORYHOOK_GATE_PROGRESS is unset.
gate_progress_emit_case() {
    local journal path outcome
    journal="$(gate_progress_journal)"
    [ -n "$journal" ] || return 0
    path="$1"
    outcome="$2"
    printf '{"kind":"case","path":"%s","outcome":"%s"}\n' \
        "$(gate_progress_json_escape "$path")" \
        "$outcome" \
        >>"$journal"
}

# Direct-invocation form, for a caller that only needs a couple of lines and
# would rather not source this file: `gate-progress.sh item <path> <status>
# [key=value...]` or `gate-progress.sh case <path> <outcome>`.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    set -uo pipefail
    case "${1:-}" in
    (item)
        shift
        gate_progress_emit_item "$@"
        ;;
    (case)
        shift
        gate_progress_emit_case "$@"
        ;;
    (*)
        echo "gate-progress.sh: usage: gate-progress.sh item <path> <status> [key=value...] | gate-progress.sh case <path> <outcome>" >&2
        exit 1
        ;;
    esac
fi
