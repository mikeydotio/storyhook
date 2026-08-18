#!/usr/bin/env bash
#
# How far is `main` from the last tree the browser suite certified? — SH-418.
#
# `make test-full` writes a `tier full` receipt; `make test` writes `tier
# gate`. Both satisfy `.githooks/pre-push` and `scripts/merge-preflight.sh`,
# which is the entire point of SH-394's split — the reduced gate is what
# protects `main`. So nothing has ever asked the *stronger* question the
# `tier` line was built to answer, and the answer on this machine when
# SH-418 was filed was stark: 109 receipts in the store, **zero** carrying
# `tier full`. The browser suite had never certified a tree here.
#
# This script is that reader. It is the primitive `scripts/browser-watch.sh`
# makes its decision with, and the one `scripts/merge-watch.sh` quotes into
# every PR comment, so the fact is in front of whoever is about to merge.
#
# WHY DISTANCE, COMPUTED PER READ, AND NEVER A CACHED MARKER. A receipt's
# absence is ambiguous on its own — "never tried" and "tried and failed" look
# identical — and the obvious repair is a marker file recording the last
# outcome. That was proposed and declined: a marker is a second notion of
# "certified" for a fact the tree-keyed store can already state, and a stale
# marker is one more thing that can be wrong about the world. Walking `main`
# back to the nearest full-certified ancestor answers the same question from
# the store itself, and answers it as a DISTANCE — so a poller that has died,
# a `main` that has been red for a day, and a machine that has never run the
# browser suite at all are three readings on one scale that only ever grows.
# Silence is never a pass; it is `never`, which is the largest reading there
# is.
#
# WHY FIRST-PARENT. `git log --first-parent` walks the trees `main` actually
# HAD, one per merge or direct commit. The commits a merge brought in were
# never `main`'s content and certifying one of them says nothing about the
# tip — which is SH-396's whole finding, that a merge tree matches neither
# parent.
#
# WHY NO STALENESS THRESHOLD. This reports; it does not judge. A ceiling on
# "how far behind is too far" would be a bare literal about one machine's
# cadence on one day, which this project already refuses in test budgets
# (SH-394). The caller decides what a distance means.
#
# USAGE
#   browser-status.sh [<ref>]     default: origin/main
#
# EXIT CODES
#   0   <ref>'s own tip tree carries a `tier full` receipt — current
#   1   an ancestor does, the tip does not — behind by N first-parent commits
#   2   no first-parent ancestor does — the browser suite has certified
#       nothing on this line of history
#
# stdout is `key=value` lines, for `merge-watch.sh` and for tests; every
# human sentence goes to stderr, in `gate-receipt.sh`'s idiom.

set -uo pipefail

die() {
    printf 'browser-status: %s\n' "$1" >&2
    exit 3
}

note() {
    printf 'browser-status: %s\n' "$1" >&2
}

ref="${1:-origin/main}"

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"
receipts="$common_dir/storyhook/gate-receipts"

git rev-parse --quiet --verify "$ref" >/dev/null 2>&1 \
    || die "'$ref' does not resolve here. Pass a ref that exists, or fetch first."

# The tier a receipt for TREE claims, or failure if there is no receipt.
#
# Deliberately fork-free: the "never" answer walks the whole first-parent
# history (641 commits in this repo when SH-418 was written), and a `grep`
# per commit would make the most common reading the slowest one.
#
# A receipt with no `tier` line predates SH-394 and certified the browser
# suite as part of the one gate that existed then. It is reported as `gate`
# anyway: what those runs covered is not recoverable from the receipt, and
# guessing generously here would let the oldest, least trustworthy receipts
# be the only ones that ever read as `full`.
receipt_tier() {
    local file="$receipts/$1" key value
    [ -f "$file" ] || return 1
    while read -r key value; do
        if [ "$key" = "tier" ]; then
            printf '%s\n' "$value"
            return 0
        fi
    done < "$file"
    printf 'gate\n'
    return 0
}

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
    if [ "$(receipt_tier "$tree" || true)" = "full" ]; then
        printf 'certified=%s\n' "$commit"
        printf 'certified_tree=%s\n' "$tree"
        printf 'certified_at=%s\n' \
            "$(date -u -r "$receipts/$tree" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
        printf 'behind=%s\n' "$behind"
        printf 'scanned=%s\n' "$scanned"
        if [ "$behind" -eq 0 ]; then
            printf 'state=current\n'
            note "current — $ref's own tree ($tip_tree) passed the browser suite"
            exit 0
        fi
        printf 'state=behind\n'
        note "behind by $behind first-parent commit(s) — the last browser-certified \
tree on this history is ${commit:0:9}'s"
        exit 1
    fi
    behind=$((behind + 1))
done < <(git log --first-parent --format='%H %T' "$ref" 2>/dev/null)

printf 'behind=%s\n' "$behind"
printf 'scanned=%s\n' "$scanned"
printf 'state=never\n'
note "never — no tree in $ref's $scanned-commit first-parent history has ever \
passed the browser suite. Run 'make browser-watch' (or 'make test-full')."
exit 2
