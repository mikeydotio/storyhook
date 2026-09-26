#!/usr/bin/env bash
# Runs one of the disjoint Rust batteries classified by rust-test-targets.sh.

set -euo pipefail

mode="${1:-}"
case "$mode" in
(core) only_flag="--only"; export STORYHOOK_GATE_PROGRESS_PATH="release gate/rust-suite" ;;
(contracts) only_flag="--only-no-doc"; export STORYHOOK_GATE_PROGRESS_PATH="release gate/rust-contracts" ;;
(*)
    echo "run-rust-battery: expected core or contracts" >&2
    exit 1
    ;;
esac

cd "$(dirname "$0")/.."

cmd=(bash scripts/run-tests.sh "$only_flag")
count=0
while IFS= read -r name; do
    [ -n "$name" ] || continue
    cmd+=("$name")
    count=$((count + 1))
done < <(bash scripts/rust-test-targets.sh "$mode")

if [ "$count" -eq 0 ]; then
    echo "run-rust-battery: $mode selected no targets; refusing a vacuous pass" >&2
    exit 1
fi

# `--test-threads=4` caps each binary; STORYHOOK_TEST_THREAD_BUDGET caps the
# test threads of all the battery's binaries together (scripts/test-pool.py,
# SH-783). The gate's default lives here, its one entry for both batteries;
# 0 restores one serial `cargo test`.
#
# 8, measured 2026-09-26: contracts 1,259 s -> 667 s, core 815 s -> 694 s, both
# green. 16 was faster again (349 s, 351 s) but failed core tests on production
# timing bounds under load (a 3 s tmux census, a 5 s spawn deadline) -- the
# stall-or-flake symptom of overshooting that the Makefile's note on
# --test-threads warns about. docs/spec/test-audit.md has the table.
export STORYHOOK_TEST_THREAD_BUDGET="${STORYHOOK_TEST_THREAD_BUDGET:-8}"
cmd+=(-- --test-threads=4)
"${cmd[@]}"
