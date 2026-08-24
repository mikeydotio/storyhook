#!/usr/bin/env bash
# Runs one of the disjoint Rust batteries classified by rust-test-targets.sh.

set -euo pipefail

mode="${1:-}"
case "$mode" in
(core) only_flag="--only" ;;
(contracts) only_flag="--only-no-doc" ;;
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

cmd+=(-- --test-threads=4)
"${cmd[@]}"
