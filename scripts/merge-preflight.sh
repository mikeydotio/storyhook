#!/usr/bin/env bash
#
# The merge gate's primitive — SH-396.
#
# `.githooks/pre-push` certifies the tip tree of a *pushed* ref. A PR merged
# with `gh pr merge --merge` is a server-side merge: no push happens, so that
# hook never fires, and this project runs no test CI in GitHub Actions by
# policy. So a merge commit on `main` can land a tree nothing has ever
# tested — and did: PR #484 (SH-315) merged two independently green
# branches with zero textual conflict into a tree that failed to compile
# (`E0004`, a variant added on one side with no arm added to an exhaustive
# match on the other). `main` was red for 73 minutes before anyone noticed.
# Measured over the last 30 merges into `main`: 14 produced a tree matching
# neither parent — content no receipt could possibly have covered.
#
# This script asks the exact question a receipt-based gate needs answered
# before a merge, not a proxy for it: does the tree this merge WOULD produce
# already carry a certification? `git merge-tree --write-tree <base> <head>`
# computes that tree without touching the working directory or creating a
# commit, and is byte-identical to what a real `git merge` of the same two
# parents would produce (verified by hand: a real merge and merge-tree of the
# same base/head agree on the resulting tree oid). Reconstructing the actual
# SH-396 incident confirms it catches the real defect: `git merge-tree
# --write-tree <main-before> <SH-315-branch-tip>` reproduces the broken
# merge's tree byte-for-byte, and that tree matches neither parent — no
# receipt could have existed for it.
#
# This script is read-only: it never writes a receipt, never touches the
# working tree, never runs the suite. It only answers "has this exact content
# gone green before". `scripts/merge-watch.sh` is what runs `make test`
# against an uncertified tree and, on green, certifies it for real.
#
# EXIT CODES
#   0   the merge is clean and its tree already carries a gate/full receipt
#   1   the merge is clean but its tree has no gate/full receipt yet -- either
#       none at all, or only a `changed`-tier one (SH-429's council verdict:
#       a selective run never certifies a merge tree, since it diffed against
#       one branch's own history and a merge is a two-parent combination
#       neither side's run ever accounted for)
#   2   the merge does not resolve cleanly (a real textual conflict, or an
#       invalid ref) — there is no tree to certify until the branch is rebased
#
# stdout carries exactly the tree oid on 0/1, and nothing on 2 (there is no
# valid tree). All commentary goes to stderr, in `gate-receipt.sh`'s idiom.

set -uo pipefail

die() {
    printf 'merge-preflight: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'merge-preflight: %s\n' "$1" >&2
}

base="${1:-}"
head="${2:-}"
[ -n "$base" ] && [ -n "$head" ] \
    || die "usage: merge-preflight.sh <base-ref> <head-ref>"

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

# Shared with `gate-receipt.sh`'s own receipt store on purpose — a merge tree
# and a pushed tree are the same kind of claim (content that passed `make
# test`), so they belong in the one content-addressed store `.githooks/
# pre-push` already reads, not a second one it would need to know about too.
common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"
receipts="$common_dir/storyhook/gate-receipts"

# On a clean merge, `--write-tree` prints exactly one line: the tree oid, and
# nothing else. On conflict it exits non-zero and stdout instead carries
# diagnostic conflict info AND a "virtual" tree oid with conflict markers
# baked in — that oid must never be treated as a real result, which is why
# this checks the exit status rather than trying to validate the first line
# as a hash.
output="$(git merge-tree --write-tree "$base" "$head" 2>&1)"
status=$?

if [ "$status" -ne 0 ]; then
    note "CONFLICT — $head does not merge cleanly onto $base"
    printf '%s\n' "$output" | sed 's/^/  /' >&2
    exit 2
fi

tree="$output"
printf '%s\n' "$tree"

if [ -f "$receipts/$tree" ]; then
    tier="$(sed -n 's/^tier //p' "$receipts/$tree" 2>/dev/null | head -n1)"
    tier="${tier:-gate}"
    # SH-429's council verdict, unanimous: a `changed`-tier receipt is never
    # sufficient to land a merge, even one that happens to exist for this
    # exact tree oid. `select-tests.sh` diffs against the nearest tree ONE
    # branch made green; a merge tree is a two-parent combination neither
    # side's own selective run ever accounted for -- the same reason SH-396
    # exists at all (14 of the last 30 merges produced a tree matching
    # neither parent, content no single-branch receipt could ever cover).
    # `gate`/`full` are unconditional (the whole suite ran), so only those
    # two certify a merge; `changed` reports as UNCERTIFIED here on purpose.
    if [ "$tier" = "gate" ] || [ "$tier" = "full" ]; then
        note "certified — tier $tier (tree $tree)"
        exit 0
    fi
    note "not certified — tree $tree carries only a '$tier' receipt, which \
merge-preflight.sh does not accept (gate/full only — SH-429)"
    exit 1
fi

note "not certified — no receipt for tree $tree"
exit 1
