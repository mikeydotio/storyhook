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
#
# `--only <name...> [-- <cargo-test-args>]` (SH-429) runs a SPECIFIC set of
# test binaries rather than the whole workspace — `scripts/select-tests.sh`'s
# own output, for `make test-changed`. A name is resolved against
# `tests/<name>.rs` first (an ordinary integration test binary); anything
# that is not one is looked up as a LIB target's own name via `cargo
# metadata` (never hardcoded — this workspace has exactly two lib crates
# today, and a name resolved from the workspace's own metadata cannot drift
# from what actually exists the way a hand-copied pair of package names
# could). A name that resolves to neither is refused (SH-357's doctrine: an
# argument that lands nowhere is refused, not silently dropped). `--only`
# with NO names at all means a deliberately empty selection — nothing to
# run — never "no filter, so run everything": that silent fallback is
# exactly the shape `select-tests.sh`'s own contract is built to avoid.
# Doctests always run regardless of `--only`, unconditionally: they are not
# part of `coverage-map.sh`'s own enumeration (a `///` example is not a
# `tests/*.rs`/lib-`#[cfg(test)]` binary), so selection has no signal for
# them at all, and they cost ~3s total (`docs/rearch/baseline/timings.md`) —
# cheap enough that skipping them was never worth the soundness gap.
# `--only-no-doc` is the same targeted mode without doctests. It is reserved
# for the checkout-contract battery: the disjoint core battery owns doctests,
# so running them again there would itself violate the no-unrelated-reruns
# contract.
set -euo pipefail

cd "$(dirname "$0")/.."

only_mode=0
run_docs=1
only_names=()
if [ "${1:-}" = "--only" ] || [ "${1:-}" = "--only-no-doc" ]; then
    only_mode=1
    [ "$1" = "--only-no-doc" ] && run_docs=0
    shift
    while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do
        only_names+=("$1")
        shift
    done
fi

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

# Every leg's combined output is teed here (terminal AND this file), so
# `scripts/test-delta.sh` can record the per-test red/green ledger at the end
# regardless of which mode ran or how many separate `cargo test` invocations
# it took. `PIPESTATUS[0]` rather than `pipefail`'s own "rightmost nonzero" —
# explicit about which command's status is being kept, since `tee` itself
# essentially never fails and conflating the two would be the wrong command
# to have decided the run's outcome.
log="$data_root/test-output.log"
: >"$log"
run_leg() {
    "$@" 2>&1 | tee -a "$log"
    return "${PIPESTATUS[0]}"
}

status=0

if [ "$only_mode" -eq 0 ]; then
    run_leg cargo test --workspace "$@" || status=$?
else
    if [ "${#only_names[@]}" -eq 0 ]; then
        echo "run-tests.sh: --only given with no binaries -- nothing to run" >&2
    else
        # Resolves a lib target's own name (e.g. storyhook_test_support) to
        # the PACKAGE name `cargo test -p` needs (storyhook-test-support) by
        # asking cargo, never by guessing the hyphen/underscore convention or
        # hardcoding the pair.
        resolve_lib_package() {
            cargo metadata --no-deps --format-version=1 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
name = sys.argv[1]
for pkg in d["packages"]:
    for t in pkg["targets"]:
        if "lib" in t["kind"] and t["name"] == name:
            print(pkg["name"])
            sys.exit(0)
sys.exit(1)
' "$1"
        }

        storyhook_test_args=()
        lib_packages=()
        for name in "${only_names[@]}"; do
            if [ -f "tests/$name.rs" ]; then
                storyhook_test_args+=(--test "$name")
                continue
            fi
            pkg="$(resolve_lib_package "$name")" || {
                echo "run-tests.sh: --only names '$name', which is neither a \
tests/*.rs binary nor a workspace lib target -- refusing rather than silently \
skipping it" >&2
                exit 1
            }
            lib_packages+=("$pkg")
        done

        if [ "${#storyhook_test_args[@]}" -gt 0 ]; then
            run_leg cargo test -p storyhook "${storyhook_test_args[@]}" "$@" || status=$?
        fi
        # `[ "${#lib_packages[@]}" -gt 0 ] && for` rather than a bare
        # `for pkg in "${lib_packages[@]}"`: bash < 4.4 (macOS's system bash
        # is 3.2, frozen there by the GPLv3) raises "unbound variable" under
        # `set -u` when expanding an EMPTY array's `[@]`, not zero iterations
        # -- a fixed, well-known portability gap, not a bug in this script's
        # own logic.
        i=0
        while [ "$i" -lt "${#lib_packages[@]}" ]; do
            pkg="${lib_packages[$i]}"
            run_leg cargo test -p "$pkg" --lib "$@" || status=$?
            i=$((i + 1))
        done
    fi
    if [ "$run_docs" -eq 1 ]; then
        run_leg cargo test --workspace --doc "$@" || status=$?
    fi
fi

# Diagnostic side-channel, never gating: a failure recording the ledger must
# never be reported as a test failure, and the tree oid may legitimately be
# unresolvable (a tarball, a corrupt index) in exactly the same cases
# `tracked-tree.sh` already tolerates for `build.rs`.
tree="$(scripts/tracked-tree.sh 2>/dev/null || true)"
if [ -n "$tree" ]; then
    bash scripts/test-delta.sh "$tree" <"$log" || true
fi

exit "$status"
