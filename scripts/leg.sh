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
# Two forms:
#   leg.sh <label> -- <cmd...>   run the command, report its wall clock,
#                                 propagate its exit status verbatim
#   leg.sh --skipped <label>     print a deferral notice instead of running
#                                 anything
#
# The deferral notice is not decoration. A gate that silently covers less
# than a reader assumes is the SH-306 shape — a green run that answers a
# question nobody asked. `make test` skips the browser suite on purpose; this
# is what says so, every time, naming the command that runs it.

set -euo pipefail

if [ "${1:-}" = "--skipped" ]; then
    label="${2:-}"
    [ -n "$label" ] || { echo "leg.sh: --skipped needs a label" >&2; exit 1; }
    echo "leg $label: SKIPPED — not part of this tier. Run \`make test-full\` to include it." >&2
    exit 0
fi

label="${1:-}"
[ -n "$label" ] || { echo "leg.sh: usage: leg.sh <label> -- <cmd...>  |  leg.sh --skipped <label>" >&2; exit 1; }
shift

if [ "${1:-}" != "--" ]; then
    echo "leg.sh: usage: leg.sh <label> -- <cmd...>" >&2
    exit 1
fi
shift

start=$(date +%s)
status=0
"$@" || status=$?
end=$(date +%s)

echo "leg $label: $((end - start))s" >&2

exit "$status"
