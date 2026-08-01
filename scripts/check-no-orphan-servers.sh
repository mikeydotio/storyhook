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
# touches it. This is the fourth layer of the orphan defence, behind the test
# guards' Drop, the daemon's own process group, and the STORYHOOK_PARENT_PID
# suicide contract; it exists to name the cause immediately if all three fail.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
phase="${1:-check}"

# `story daemon --serve` is the daemon; `story web --serve` is its alias, which
# a test may still be spelling. Both are matched, and so are the two test
# binaries that run servers in-thread.
#
# `([^ ]+ )*` between the binary and the verb is not decoration: a spawned
# daemon carries `--store-path <file>` ahead of its verb (SH-113), so a pattern
# anchored on `story daemon --serve` stopped matching the very processes this
# guard exists to find -- and a guard that matches nothing passes.
pattern="${repo_root}/target/debug/(deps/(web_test|daemon_lifecycle|daemon_invoke|storyhook_test_support)-|story ([^ ]+ )*(daemon|web) --serve)"

# The postlude waits; the preflight does not.
#
# A daemon started by a test binary exits when that binary does, but it learns
# of it by polling, so for a moment after `cargo test` returns there can still
# be one winding down. Failing on that would be failing on the defence working.
# A daemon still there after the grace period is a real leak: nothing is going
# to collect it.
if [ "$phase" = "postlude" ]; then
  deadline=$(( SECONDS + 10 ))
  while [ "$SECONDS" -lt "$deadline" ]; do
    orphans="$(pgrep -f "$pattern" || true)"
    [ -z "$orphans" ] && exit 0
    sleep 0.25
  done
fi

orphans="$(pgrep -f "$pattern" || true)"

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
