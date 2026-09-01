#!/usr/bin/env bash
#
# Times and labels one leg of `make test` / `make test-full` — SH-394.
#
# The gate used to be one undifferentiated block of Makefile recipe lines, so
# nobody had numbers for which leg cost what. This is the evidence base for
# that: every leg's wall clock lands on stderr, labelled, so the next person
# who wants to move a leg between tiers has a measurement instead of a
# memory — the way SH-222 had numbers before it moved a budget.
#
# Three forms:
#   leg.sh <label> -- <cmd...>   run the command, report its wall clock,
#                                 propagate its exit status verbatim
#   leg.sh --reuse <label> -- <cmd...>
#                                reuse a prior successful result while this
#                                leg's tracked inputs and argv are identical;
#                                otherwise run and record it on success
#   leg.sh --skipped <label>     print a deferral notice instead of running
#                                 anything
#
# The deferral notice is not decoration. A gate that silently covers less
# than a reader assumes is the SH-306 shape — a green run that answers a
# question nobody asked. `make test` skips the browser suite on purpose; this
# is what says so, every time, naming the command that runs it.
#
# Every leg boundary below also emits a "release gate/<label>" item to the
# SH-524 gate progress journal (scripts/gate-progress.sh), a no-op unless
# $STORYHOOK_GATE_PROGRESS is set. leg.sh is the single choke point every one
# of the seven legs in Makefile's `_test-body`/`_test-changed-body` passes
# through, so it is the one place that boundary needs stating.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=gate-progress.sh
. "$script_dir/gate-progress.sh"

if [ "${1:-}" = "--skipped" ]; then
    label="${2:-}"
    [ -n "$label" ] || { echo "leg.sh: --skipped needs a label" >&2; exit 1; }
    echo "leg $label: SKIPPED — not part of this tier. Run \`make test-full\` to include it." >&2
    gate_progress_emit_item "release gate/$label" skipped
    exit 0
fi

reuse=0
if [ "${1:-}" = "--reuse" ]; then
    reuse=1
    shift
fi

label="${1:-}"
[ -n "$label" ] || { echo "leg.sh: usage: leg.sh [--reuse] <label> -- <cmd...>  |  leg.sh --skipped <label>" >&2; exit 1; }
shift

if [ "${1:-}" != "--" ]; then
    echo "leg.sh: usage: leg.sh [--reuse] <label> -- <cmd...>" >&2
    exit 1
fi
shift

fingerprint=""
receipt=""
if [ "$reuse" = 1 ]; then
    case "$label" in
    (*[!a-z0-9-]* | '')
        echo "leg.sh: reusable label must contain only lowercase letters, digits, and hyphens" >&2
        exit 1
        ;;
    esac

    root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
        echo "leg.sh: --reuse requires a git worktree" >&2
        exit 1
    }
    common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" || {
        echo "leg.sh: could not resolve the shared git directory" >&2
        exit 1
    }
    fingerprint="$("$root/scripts/gate-leg-fingerprint.sh" "$label" "$@")" || {
        echo "leg.sh: could not fingerprint reusable leg $label" >&2
        exit 1
    }
    receipt_dir="$common_dir/storyhook/gate-leg-receipts/$label"
    receipt="$receipt_dir/$fingerprint"

    if [ -f "$receipt" ] \
        && grep -q "^fingerprint $fingerprint$" "$receipt" 2>/dev/null; then
        echo "leg $label: REUSED — relevant tracked inputs and command are unchanged" >&2
        gate_progress_emit_item "release gate/$label" reused
        exit 0
    fi
fi

gate_progress_emit_item "release gate/$label" running
start=$(date +%s)
status=0
"$@" || status=$?
end=$(date +%s)
elapsed=$((end - start))

echo "leg $label: ${elapsed}s" >&2
gate_progress_emit_item "release gate/$label" "$([ "$status" = 0 ] && echo passed || echo failed)" "seconds=$elapsed"

if [ "$reuse" = 1 ] && [ "$status" = 0 ]; then
    after="$("$root/scripts/gate-leg-fingerprint.sh" "$label" "$@")" || {
        echo "leg.sh: could not re-fingerprint successful leg $label; result is not reusable" >&2
        exit 1
    }
    if [ "$fingerprint" != "$after" ]; then
        echo "leg $label: relevant tracked inputs changed while it ran; result was not recorded" >&2
    else
        mkdir -p "$receipt_dir"
        tmp="$receipt_dir/.tmp.$$"
        {
            printf 'fingerprint %s\n' "$fingerprint"
            printf 'label %s\n' "$label"
            printf 'worktree %s\n' "$root"
        } >"$tmp"
        mv -f "$tmp" "$receipt"
    fi
fi

exit "$status"
