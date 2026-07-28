#!/usr/bin/env bash
#
# Fails when a server this repo's test suite starts is still running.
#
# Test-spawned servers that outlive their run are not a curiosity: they hold
# their ports, and a later run that lands on one of those ports talks to a
# stranger's registry, which answers 404 to everything it is asked. That is
# the failure mode of SH-51 -- 78 of 139 tests down, with nothing in the
# output pointing at the real cause. Ports are now kernel-assigned and daemons
# are stopped by a guard, so this should never fire; it exists to name the
# cause immediately if the class ever comes back.
#
# Only ever reports processes built from THIS working tree (target/debug/...).
# The developer's real dashboard daemon -- an installed `story` binary, port
# 3456 -- is deliberately out of scope: it is production, and nothing here
# touches it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
phase="${1:-check}"

orphans="$(pgrep -f "${repo_root}/target/debug/(deps/web_test-|story web --serve)" || true)"

if [ -z "$orphans" ]; then
  exit 0
fi

echo "error: test-spawned server processes from this worktree are still running (${phase}):" >&2
# shellcheck disable=SC2086 # deliberate word splitting: one -p per pid list
ps -o pid,etime,command -p $(echo "$orphans" | tr '\n' ',' | sed 's/,$//') >&2
cat >&2 <<EOF

They hold ports that a later run can be handed, and a test that reaches one of
them sees an empty registry (spurious 404s everywhere). Stop them, then re-run:

  kill $(echo "$orphans" | tr '\n' ' ')
EOF
exit 1
