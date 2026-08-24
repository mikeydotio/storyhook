#!/usr/bin/env bash
#
# How far is `main` from the last tree `coverage-map.sh` captured? — SH-429.
#
# The exact reader `docs/spec/test-tiers.md`'s "The push gate narrowed"
# section and this story's council verdict both point at: `scripts/select-
# tests.sh` falls back to running everything whenever the tree it resolves as
# the nearest fully-certified baseline has no coverage map, so a stale or
# absent map costs wall-clock, never soundness (SH-429's own council verdict,
# Q3) — but "costs wall-clock, never soundness" only STAYS true if something
# is watching that cost and keeping it small. This is that reader, the same
# role `scripts/browser-status.sh` already plays for the browser tier, and
# deliberately the same shape: distance, computed per read, never a cached
# marker (a marker is a second notion of "captured" for a fact the map store
# can already state, and a stale marker is one more thing to be wrong about).
#
# WHY FIRST-PARENT. Identical reasoning to `browser-status.sh`: `git log
# --first-parent` walks the trees `main` actually HAD; a commit a merge
# brought in was never main's own content, and a map captured against it says
# nothing about main's tip.
#
# WHY NO STALENESS THRESHOLD. This reports; it does not judge, for the same
# reason `browser-status.sh` does not — a ceiling on "how stale is too stale"
# is a bare literal about one machine's cadence on one day (CLAUDE.md,
# SH-394). The caller (`select-tests.sh`, a human reading `make coverage-
# status`) decides what the distance means.
#
# USAGE
#   coverage-status.sh [<ref>]     default: origin/main
#
# EXIT CODES
#   0   <ref>'s own tip tree carries a coverage map — current
#   1   an ancestor's does, the tip's does not — behind by N commits
#   2   no first-parent ancestor's does — coverage has never been captured on
#       this line of history
#   3   could not answer at all (unresolvable ref, not a git worktree)
#
# stdout is `key=value` lines, for `scripts/coverage-watch.sh` and for tests;
# every human sentence goes to stderr, in `gate-receipt.sh`'s idiom.

set -uo pipefail

die() {
    printf 'coverage-status: %s\n' "$1" >&2
    exit 3
}

note() {
    printf 'coverage-status: %s\n' "$1" >&2
}

ref="${1:-origin/main}"

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"
maps_dir="$common_dir/storyhook/coverage-maps"

git rev-parse --quiet --verify "$ref" >/dev/null 2>&1 \
    || die "'$ref' does not resolve here. Pass a ref that exists, or fetch first."

tip="$(git rev-parse "$ref^{commit}" 2>/dev/null)" \
    || die "'$ref' does not name a commit"
tip_tree="$(git rev-parse "$ref^{tree}" 2>/dev/null)" \
    || die "cannot resolve a tree for '$ref'"

printf 'ref=%s\n' "$ref"
printf 'tip=%s\n' "$tip"
printf 'tip_tree=%s\n' "$tip_tree"

behind=0
scanned=0
while read -r commit tree; do
    [ -n "${commit:-}" ] || continue
    scanned=$((scanned + 1))
    if [ -f "$maps_dir/$tree" ]; then
        printf 'certified=%s\n' "$commit"
        printf 'certified_tree=%s\n' "$tree"
        printf 'certified_at=%s\n' \
            "$(date -u -r "$maps_dir/$tree" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
        printf 'behind=%s\n' "$behind"
        printf 'scanned=%s\n' "$scanned"
        if [ "$behind" -eq 0 ]; then
            printf 'state=current\n'
            note "current — $ref's own tree ($tip_tree) has a coverage map"
            exit 0
        fi
        printf 'state=behind\n'
        note "behind by $behind first-parent commit(s) — the last coverage-mapped \
tree on this history is ${commit:0:9}'s"
        exit 1
    fi
    behind=$((behind + 1))
done < <(git log --first-parent --format='%H %T' "$ref" 2>/dev/null)

printf 'behind=%s\n' "$behind"
printf 'scanned=%s\n' "$scanned"
printf 'state=never\n'
note "never — no tree in $ref's $scanned-commit first-parent history has ever \
had a coverage map captured. Run 'make coverage-watch' (or 'make coverage-map')."
exit 2
