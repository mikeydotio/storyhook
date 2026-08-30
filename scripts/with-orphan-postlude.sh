#!/usr/bin/env bash
#
# Runs one command, then always runs the test-daemon orphan postlude (SH-491).
#
# `make` stops at the first failing recipe line. That is exactly what keeps a
# red suite from reaching the receipt writer, but it also used to skip the
# orphan postlude and leave this run's daemons for the next preflight to
# refuse. This wrapper makes cleanup unconditional without weakening the
# receipt: the Makefile keeps the receipt on the following recipe line, so it
# remains unreachable unless both the body and this postlude succeed.
#
# Status precedence is deliberate. A red body keeps its own status so the
# failing test leg remains the primary diagnosis. A green body adopts a red
# postlude's status, preventing a receipt over a process the run could not
# clean up. Either way, the postlude's stderr remains visible.
#
# `--make-no-exec` is for the recursive recipe's -n/-t/-q path only. GNU Make
# executes a line containing `$(MAKE)` even in those modes, so the recursive
# command still has to run; the real postlude emphatically does not. The
# Makefile derives this flag from MAKEFLAGS using GNU Make's documented test.

set -uo pipefail

die() {
    printf 'with-orphan-postlude: %s\n' "$1" >&2
    exit 2
}

make_no_exec=0
if [ "${1:-}" = "--make-no-exec" ]; then
    make_no_exec=1
    shift
fi

[ "${1:-}" = "--" ] \
    || die "usage: with-orphan-postlude.sh [--make-no-exec] -- command [args...]"
shift
[ "$#" -gt 0 ] || die "'--' must be followed by a command to run"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" \
    || die "could not resolve the repository root"

body_status=0
"$@" || body_status=$?

if [ "$make_no_exec" -eq 1 ]; then
    exit "$body_status"
fi

postlude_status=0
bash "$root/scripts/check-no-orphan-servers.sh" postlude || postlude_status=$?

if [ "$body_status" -ne 0 ]; then
    exit "$body_status"
fi
exit "$postlude_status"
