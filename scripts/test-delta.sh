#!/usr/bin/env bash
#
# The per-test red/green ledger — SH-429.
#
# Reads `cargo test`'s own default text output from stdin (never `--format
# json`; the ledger has to work for `run-tests.sh`'s ordinary invocation, and
# a caller that wants the raw output preserved on the terminal too pipes
# through `tee` before this, e.g. `cargo test ... | tee /dev/tty | bash
# scripts/test-delta.sh "$tree"`), records PASS/FAIL for every individual
# test function this run actually executed, and reports how that compares to
# the nearest ancestor commit that has its own ledger.
#
# THE LEDGER is `$(git rev-parse --git-common-dir)/storyhook/test-results/
# <tree-oid>` — the same storage family as `gate-receipt.sh`'s receipts and
# `coverage-map.sh`'s maps, for the same reasons: shared across worktrees by
# content, inside `.git/` so a fresh clone starts with none.
#
# WHAT "not re-run since" MEANS, AND WHY IT IS NEVER SILENTLY COUNTED AS
# GREEN. A `make test-changed` run executes a SUBSET of the suite. A test
# that passed in the comparison ledger but does not appear in THIS run's
# ledger was not re-verified — it is not known-green, it is UNKNOWN, and
# reporting it as green would be exactly the silent optimism this project's
# own doctrine forbids (CLAUDE.md: SH-312, SH-345, SH-364). So the four
# buckets below are exhaustive and a test lands in exactly one:
#
#   newly-red       ran this time, FAILED; passed or was absent before
#   newly-green     ran this time, PASSED; FAILED before
#   still-red       ran this time, FAILED; also FAILED before
#   not-re-run-since  did NOT run this time; was recorded (either PASS or
#                     FAIL) in the comparison ledger — status as of THIS run
#                     is simply not known, and the report says so by name
#
# A test that ran and passed both times, or that never appeared in the
# comparison ledger and passed this time, is not reported — it is the
# unremarkable case, and this report exists to surface what changed, not to
# restate the whole suite.
#
# Usage: test-delta.sh <tree-oid>   (reads cargo test output from stdin)

set -uo pipefail

die() {
    printf 'test-delta: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'test-delta: %s\n' "$1" >&2
}

tree="${1:-}"
[ -n "$tree" ] || die "usage: test-delta.sh <tree-oid>  (reads cargo test output from stdin)"

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"

results_dir="$common_dir/storyhook/test-results"
mkdir -p "$results_dir" || die "could not create $results_dir"

work="$(mktemp -d /private/tmp/storyhook-test-delta.XXXXXX)" \
    || die "could not create a scratch directory"
trap 'rm -rf "$work"' EXIT

# ---------------------------------------------------------------------------
# Parse cargo test's own text output into <binary>\t<test>\t<PASS|FAIL>
# ---------------------------------------------------------------------------
#
# The grammar itself lives in test-progress.awk (SH-524), shared with the gate
# progress journal so the two readers of cargo's text output cannot drift.
awk -f "$root/scripts/test-progress.awk" >"$work/current.tsv"

LC_ALL=C sort -u "$work/current.tsv" >"$work/current.sorted.tsv"

ledger_tmp="$results_dir/.tmp.$$"
cp "$work/current.sorted.tsv" "$ledger_tmp" || die "could not stage the ledger"
mv -f "$ledger_tmp" "$results_dir/$tree" || die "could not publish the ledger"

count="$(wc -l <"$work/current.sorted.tsv" | tr -d ' ')"
note "recorded $count test outcomes for tree $tree"

# ---------------------------------------------------------------------------
# Find the nearest ancestor commit with its own ledger, and diff against it
# ---------------------------------------------------------------------------

comparison_tree=""
# shellcheck disable=SC2162
while read -r commit ctree; do
    [ -n "$commit" ] || continue
    [ "$ctree" = "$tree" ] && continue
    if [ -f "$results_dir/$ctree" ]; then
        comparison_tree="$ctree"
        break
    fi
done < <(git log --first-parent --max-count=2000 --format='%H %T' HEAD 2>/dev/null)

if [ -z "$comparison_tree" ]; then
    note "no prior ledger found among HEAD's ancestors -- nothing to compare against"
    exit 0
fi

note "comparing against tree $comparison_tree"

python3 - "$work/current.sorted.tsv" "$results_dir/$comparison_tree" <<'PYEOF'
import sys

def load(path):
    rows = {}
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            if not line:
                continue
            binary, name, outcome = line.split("\t", 2)
            rows[(binary, name)] = outcome
    return rows

current = load(sys.argv[1])
comparison = load(sys.argv[2])

newly_red = []
newly_green = []
still_red = []
not_rerun = []

for key, outcome in sorted(current.items()):
    prior = comparison.get(key)
    if outcome == "FAIL":
        if prior == "FAIL":
            still_red.append(key)
        else:
            newly_red.append(key)
    else:
        if prior == "FAIL":
            newly_green.append(key)

current_keys = set(current)
for key, prior in sorted(comparison.items()):
    if key not in current_keys:
        not_rerun.append((key, prior))

def report(label, items):
    if not items:
        return
    print(f"test-delta: {label} ({len(items)}):", file=sys.stderr)
    for item in items:
        if isinstance(item, tuple) and len(item) == 2 and isinstance(item[0], tuple):
            (binary, name), prior = item
            print(f"  {binary}::{name} (was {prior})", file=sys.stderr)
        else:
            binary, name = item
            print(f"  {binary}::{name}", file=sys.stderr)

report("newly RED", newly_red)
report("newly green", newly_green)
report("still red", still_red)
report("not re-run since the comparison ledger -- status unknown, not assumed green", not_rerun)

if not (newly_red or newly_green or still_red or not_rerun):
    print("test-delta: no change from the comparison ledger", file=sys.stderr)
PYEOF
