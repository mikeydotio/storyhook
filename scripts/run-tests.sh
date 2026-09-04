#!/usr/bin/env bash
#
# `cargo test --workspace`, with an isolated storyhook data directory.
#
# **This wrapper is load-bearing, not tidy.** Story data lives in one global
# database per machine, so a run that names no store writes into the
# developer's real `~/.local/share/storyhook/store.db` — real projects, real
# stories, in the database their own `story` commands read.
#
# It used to be the ONLY thing standing between this suite and that store:
# ~45 integration-test files built fixtures with `tempfile::tempdir()` and ran
# the binary with this process's environment inherited. They are all on
# `storyhook_test_support::TestEnv` now, which isolates each `story` child
# itself, and `tests/fixture_isolation.rs` refuses the forty-fourth. So this
# block is defence in depth rather than the whole defence — which is the
# correct amount of defence for the thing it protects, not a reason to remove
# it: it also covers the daemon, the state home and anything a future fixture
# does before it reaches the harness.
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

# `$0` is resolved to an absolute path BEFORE the `cd` below, because the lock
# block re-execs this script by name and a relative `$0` stops resolving the
# moment the working directory moves.
self="$0"
case "$self" in
(/*) ;;
(*) self="$PWD/$self" ;;
esac

script_dir="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=gate-progress.sh
. "$script_dir/gate-progress.sh"

cd "$(dirname "$0")/.."

# THE MACHINE-WIDE `gate` LOCK -- SH-457, decision D4 of
# `docs/spec/full-auto-engine.md`.
#
# WHY. This suite is 36.375s median warm and idle
# (`docs/rearch/baseline/timings.md`) and has been measured at 873s under the
# three-to-four concurrent worktree suites this machine routinely runs.  That
# contention is the documented cause of an open class of load-sensitive
# failures (SH-347, SH-349, SH-375, SH-378, SH-401, SH-419), and Full Auto
# (SH-452) runs N agent lanes at once, multiplying exactly it.  So the suites
# queue instead of piling up.
#
# WHY INTERACTIVE RUNS ARE NOT EXEMPT, recorded so it is not re-litigated: a
# human's suite contends identically with a lane's.  Exempting it would leave
# the hole open in exactly the case where somebody is present to be confused
# by the result.
#
# WHY HERE, rather than around the `make test` recipe: every caller queues,
# including `scripts/run-rust-battery.sh`, `scripts/run-changed.sh` and a bare
# `bash scripts/run-tests.sh` typed by hand.  A rule that lived in the Makefile
# would cover the one door and none of the others.
#
# AND WHY THIS IS THE FLOOR RATHER THAN THE WHOLE STORY.  `make test` reaches
# this script TWICE -- once per disjoint Rust battery -- so two concurrent
# `make test` runs interleave at the battery boundary: only one `cargo test`
# ever exists on this machine, but the two runs are not whole-run exclusive and
# their fmt/clippy/build/plugin legs still overlap.  Whole-run exclusivity is
# one wrap away and needs nothing new:
#
#     scripts/machine-lock.sh gate -- make test
#
# which is the case `machine-lock.sh`'s reentrancy branch was built for, and
# which the engine's lanes use.  `scripts/run-e2e.sh` does not take this lock
# either; the browser leg is outside D4's wording.
#
# NO `--max-wait`. Waiting for a holder is bounded by the FACT of whether that
# process is still alive, never by a clock. The holder itself is different:
# `machine-lock.sh` applies SH-536's default `--max-idle` to the reserved gate
# name and resets it on every SH-524 journal append. That bounds a lack of
# progress without bounding how long a progressing suite may run.
#
# REENTRANCY IS `machine-lock.sh`'S TO DECIDE, NOT THIS SCRIPT'S.  A run whose
# own process tree already holds `gate` -- an operator or an engine lane having
# wrapped a whole `make test` -- must not wait on itself, and the wrapper
# already has that branch, reports taking it, and is tested on it.  This script
# re-execs unconditionally and lets the wrapper answer, rather than reading
# `STORYHOOK_MACHINE_LOCKS` here too: the format of that variable would then be
# known in two places, which is the SH-136 shape this project has already paid
# for four times.  Measured rather than argued -- with a second guard written
# here, mutating it away changed no observable at all, because the wrapper was
# answering anyway.  The cost of not having it is one `bash` that immediately
# execs.
#
# `STORYHOOK_GATE_LOCK_TAKEN` is the handshake across this script's own
# re-exec, and it exists so the ORDINARY path -- by far the common one -- says
# nothing at all.  The bypass, by contrast, is reported always: a bypass nobody
# can see is the SH-306 shape this project has already paid for once, with
# `SKIP_PREPUSH_TESTS`.
#
# The re-exec sits ABOVE everything this script would otherwise have to clean
# up -- a queued run holds no temp data root and no EXIT trap it has not yet
# installed.
if [ -n "${STORYHOOK_GATE_LOCK_TAKEN:-}" ]; then
    # This run took the lock a moment ago, in the `else` branch below. The
    # handshake is between those two adjacent processes and nobody else, so it
    # is consumed here rather than left in the environment of every test
    # binary the suite is about to start -- one of which could otherwise
    # invoke this script and silently skip the lock while believing it held
    # one.
    unset STORYHOOK_GATE_LOCK_TAKEN STORYHOOK_GATE_LOCK_DEPTH
elif [ "${STORYHOOK_GATE_LOCK:-1}" = "0" ]; then
    echo "run-tests.sh: STORYHOOK_GATE_LOCK=0 -- running WITHOUT the machine-wide 'gate' lock; a concurrent suite will contend with this one" >&2
else
    # THE DEPTH GUARD, AND WHY IT IS NOT THE SAME CHECK TWICE.  Arriving here
    # a second time means the handshake above did not land -- and the failure
    # mode of that is not a hang but a FORK BOMB, because `machine-lock.sh`
    # runs its command in a background child (it has to, or a signal could not
    # reach it during a fifteen-minute suite), so each cycle leaves a live
    # process waiting on the next.  Measured: roughly two hundred processes a
    # second, on a machine that routinely runs three or four other suites.
    #
    # So the two halves live at two sites and neither can cover for the other,
    # SH-365's shape: break the consumption above and this refuses by name,
    # break this and the mutation is caught by the tests that provoke the
    # ordinary path.  Refusing rather than looping is SH-357's doctrine one
    # axis over -- a state that cannot be honoured is refused, never absorbed.
    depth=$(( ${STORYHOOK_GATE_LOCK_DEPTH:-0} + 1 ))
    if [ "$depth" -gt 1 ]; then
        echo "run-tests.sh: reached the 'gate' lock take $depth times over, which means the re-exec handshake is not landing -- refusing rather than forking a process per cycle" >&2
        exit 2
    fi
    STORYHOOK_GATE_LOCK_TAKEN=1 STORYHOOK_GATE_LOCK_DEPTH="$depth" \
        exec bash scripts/machine-lock.sh gate -- bash "$self" "$@"
fi

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

# THE ISOLATION, in one shared place -- `scripts/test-env.sh`, whose own
# header carries the parameters and the reason for each. `--home` is
# deliberately NOT passed: this script wraps `cargo`, and cargo with a fake
# $HOME loses its registry and its build cache. `storyhook_test_support`'s
# `TestEnv` redirects $HOME on each `story` child instead, which is the level a
# wrapper around cargo cannot reach and a test binary can.
#
# The refusal that used to live here -- a `case` on $STORYHOOK_DATA_DIR -- is
# now `storyhook_isolate`'s own, applied to the root before anything is
# derived from it, so every harness gets it rather than the three that
# happened to have copied it.
#
# There is still a second guard inside the binary, and the two cover different
# holes: `storyhook::env::is_test_build` refuses to *resolve* a data home a
# test build was not given, which is the case a bare `cargo test` produces and
# no wrapper script can reach.
# shellcheck source=test-env.sh
. "$script_dir/test-env.sh"
storyhook_isolate "$data_root"

export INSTA_UPDATE=no

# Every leg's combined output is teed here (terminal AND this file), so
# `scripts/test-delta.sh` can record the per-test red/green ledger at the end
# regardless of which mode ran or how many separate `cargo test` invocations
# it took. `PIPESTATUS[0]` rather than `pipefail`'s own "rightmost nonzero" —
# explicit about which command's status is being kept, since `tee` itself
# essentially never fails and conflating the two would be the wrong command
# to have decided the run's outcome.
log="$data_root/test-output.log"
: >"$log"

# SH-524: live per-test progress. `$STORYHOOK_GATE_PROGRESS_PATH` names which
# checklist item ("release gate/rust-suite" or "release gate/rust-contracts")
# this invocation's cases nest under -- set by run-rust-battery.sh per mode,
# defaulting to the rust-suite leg's own path for run-changed.sh's selective
# call, which is always that leg under `make test-changed`. This script emits
# ONLY "case" lines, never "item" lines: leg.sh already owns the item
# lifecycle (running/passed/failed) for both paths, since it wraps this
# script's whole invocation.
gate_progress_case_path="${STORYHOOK_GATE_PROGRESS_PATH:-release gate/rust-suite}"
run_leg() {
    if [ -n "$(gate_progress_journal)" ]; then
        # `env -u` strips the journal (and its path hint) from cargo and every
        # test binary it spawns -- the same containment idiom
        # scripts/merge-watch.sh already uses for STORYHOOK_STORE_PATH. A
        # nested test that shells out to leg.sh/gate-receipt.sh against a
        # disposable fixture repo (tests/gate_leg_reuse.rs and siblings) must
        # not see this run's own journal path, or its fixture-scoped emissions
        # would interleave into THIS run's real journal.
        env -u STORYHOOK_GATE_PROGRESS -u STORYHOOK_GATE_PROGRESS_PATH "$@" 2>&1 \
            | tee -a "$log" \
            | awk -f "$script_dir/test-progress.awk" \
            | while IFS=$'\t' read -r _bin _name outcome; do
                gate_progress_emit_case "$gate_progress_case_path" \
                    "$([ "$outcome" = PASS ] && echo pass || echo fail)"
            done
        return "${PIPESTATUS[0]}"
    fi
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
