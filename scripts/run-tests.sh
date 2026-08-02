#!/usr/bin/env bash
#
# `cargo test --workspace`, with an isolated storyhook data directory.
#
# **This wrapper is load-bearing, not tidy.** Story data lives in one global
# database per machine. Roughly 45 integration-test files still build their
# fixtures with `tempfile::tempdir()` and run the `story` binary with this
# process's environment inherited, so without an override every one of them
# writes into the developer's real `~/.local/share/storyhook/store.db` — real
# projects, real stories, in the database the developer's own `story` commands
# read. `storyhook_test_support::TestEnv` isolates the tests that use it and
# sets this variable again with its own value; this covers everything else.
#
# `/private/tmp` rather than `$TMPDIR`: the latter is Spotlight-indexed on
# macOS, and a full run creates a database plus a write-ahead log per test
# binary (SH-53).
#
# `INSTA_UPDATE=no` makes the golden CLI corpus a real gate: insta's default is
# to write a `.snap.new` beside any snapshot that no longer matches, and a
# developer who then runs `cargo insta accept` has silently rewritten the
# byte-compatibility contract the whole rearchitecture is measured against.
set -euo pipefail

cd "$(dirname "$0")/.."

data_root="$(mktemp -d /private/tmp/storyhook-gate.XXXXXX)"
trap 'rm -rf "$data_root"' EXIT

export STORYHOOK_DATA_DIR="$data_root/data"
export XDG_STATE_HOME="$data_root/state"

# `STORYHOOK_STORE_PATH` outranks `STORYHOOK_DATA_DIR` (SH-113). A developer who
# has one exported -- which is exactly what somebody debugging a second store
# would have -- would otherwise run this whole suite against it, and the guard
# below would not notice, because it inspects the variable that lost.
unset STORYHOOK_STORE_PATH
export INSTA_UPDATE=no

# Never the production port.
export STORYHOOK_DAEMON_ADDR="${STORYHOOK_DAEMON_ADDR:-127.0.0.1:0}"

# A daemon this run starts must not outlive it, however this run ends.
export STORYHOOK_PARENT_PID="$$" 

# The guard, because the consequence of losing the override is silent and
# expensive: a store path under the real home means a test run is about to eat
# real data.
#
# There is a second guard inside the binary — `storyhook::env::is_test_build`
# refuses to *resolve* a data home a test build was not given — and the two
# cover different holes. This one catches an override that points somewhere
# real; that one catches the absence of an override at all, which is the case a
# bare `cargo test` produces and no wrapper script can reach.
case "$STORYHOOK_DATA_DIR" in
    /private/tmp/*) ;;
    *)
        echo "run-tests.sh: refusing to run with STORYHOOK_DATA_DIR=$STORYHOOK_DATA_DIR" >&2
        echo "  the gate must never point at a real storyhook store" >&2
        exit 1
        ;;
esac

cargo test --workspace "$@"
