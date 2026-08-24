#!/usr/bin/env bash
#
# The rust-suite leg of `make test-changed` — SH-429.
#
# Asks `scripts/select-tests.sh` what to run, runs it through `scripts/run-
# tests.sh`, and — on success only — records which receipt tier the run
# actually earned for the Makefile's own postlude line to use. It does NOT
# write the receipt itself: `gate-receipt.sh postlude` stays the LAST line of
# the `test-changed` recipe, exactly as it is for `test`, so "no receipt
# unless every leg passed" (build, the plugin harness, the orphan check too)
# stays true by make's own fail-fast semantics rather than by this script's
# own say-so (`tests/push_gate.rs::the_makefile_enrolls_first_and_certifies_
# last` pins the shape for `test`; `tests/selective_gate.rs` pins it for this
# target too).
#
# THE TIER IS HONEST, NOT ASPIRATIONAL. `select-tests.sh` can answer `ALL`
# for reasons that have nothing to do with staleness (no baseline found, a
# changed path outside src/**.rs) as well as for staleness itself — and in
# EVERY one of those cases, the whole suite just ran, so the receipt this
# earns is `gate`, never `changed`. Only an actual SUBSET run earns `changed`,
# and only with the `base` tree `select-tests.sh` resolved. This is the exact
# tier-honesty rule the SH-429 council settled on for the staleness case
# specifically (`gate-receipt.sh`'s own postlude enforces the mechanics of a
# `changed` receipt needing a base with its own gate/full receipt); this
# script generalizes it to every escape hatch that runs everything, not just
# the stale-map one.
#
# Writes its verdict to `$(git rev-parse --git-dir)/storyhook-changed-tier-
# args` — transient, per-worktree state, the same placement `gate-receipt.sh`
# already uses for its own preflight marker, and for the same reason: two
# worktrees running `make test-changed` at once must not read each other's.

set -uo pipefail

die() {
    printf 'run-changed: %s\n' "$1" >&2
    exit 1
}

root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

git_dir="$(git rev-parse --git-dir 2>/dev/null)" || die "cannot resolve this worktree's git directory"
state_file="$git_dir/storyhook-changed-tier-args"
rm -f "$state_file"

select_out="$(bash scripts/select-tests.sh)" || die "select-tests.sh failed"

baseline="$(printf '%s\n' "$select_out" | sed -n '1s/^BASELINE //p')"
# Line 2 is a PEEK, not a consumed field: `select-tests.sh`'s own contract
# names no separate "directive" line, only `ALL` or the selection itself
# starting at line 2. Checking it here and then still including it in
# `names` below (via `tail -n +2`, not `+3`) is what keeps the first selected
# binary from being silently dropped.
directive="$(printf '%s\n' "$select_out" | sed -n '2p')"
names="$(printf '%s\n' "$select_out" | tail -n +2)"

if [ "$directive" = "ALL" ]; then
    # Checkout-reading integration tests are a separate reusable battery.
    # Running the whole workspace here would duplicate them immediately
    # before Makefile's rust-contracts leg.
    bash scripts/run-rust-battery.sh core
    status=$?
    if [ "$status" -eq 0 ]; then
        printf 'gate\n' >"$state_file"
    fi
    exit "$status"
fi

# The rust-contracts leg owns every checkout-reading integration target. Drop
# those names from this selective core invocation so no target can execute in
# both batteries during one gate.
contracts="$(bash scripts/rust-test-targets.sh contracts)" || die "could not classify Rust contract targets"
names="$(printf '%s\n' "$names" | grep -Fvx -f <(printf '%s\n' "$contracts") || true)"

# `cmd` starts non-empty (4 elements) and only ever grows, so `"${cmd[@]}"`
# below never expands a truly-empty array -- bash < 4.4 (macOS's system bash
# is 3.2) raises "unbound variable" under `set -u` for that specific case,
# not for zero loop iterations, which is why the names are appended onto an
# already-nonempty array rather than expanded from a separately-built one.
cmd=(bash scripts/run-tests.sh --only)
while IFS= read -r n; do
    [ -n "$n" ] && cmd+=("$n")
done <<<"$names"
cmd+=(-- --test-threads=4)

"${cmd[@]}"
status=$?
if [ "$status" -eq 0 ]; then
    printf 'changed %s\n' "$baseline" >"$state_file"
fi
exit "$status"
