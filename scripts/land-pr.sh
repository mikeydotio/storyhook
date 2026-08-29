#!/usr/bin/env bash
#
# Certify and land one autonomous-lane PR under the machine-wide merge lock.
#
#   land-pr.sh <pr>
#
# The public path takes `machine-lock.sh merge` before it reads the PR, fetches
# refs, or asks `merge-preflight.sh` whether the exact merge tree is green.
# The two private modes are self-entry points, not additional user interfaces:
# `--locked` owns the live GitHub orchestration and `--certified-run` is the
# small, real-git core tests can provoke without mocking GitHub.
#
# WHY THE TOOL OWNS THE LOCK. Full Auto can run several lane agents at once.
# Asking each agent to remember a lock around its bare `gh pr merge` is the
# SH-306 shape: a gate that can silently not run. Every lane instead names this
# script, and the script itself serializes certification immediately beside
# the merge (SH-452 decision D5).
#
# WHY THE HEAD IS MATCHED AND THE LANDED TREE IS CHECKED. `gh pr merge` offers
# an expected-head guard, so a branch update cannot make the command merge
# content other than what was certified. It offers no expected-base guard;
# SH-474 tracks that platform-level race separately. Fetching the reported
# merge commit and comparing its tree with STORYHOOK_CERTIFIED_MERGE_TREE makes
# such a race a loud hard failure rather than an asserted success. The remote
# source branch is deleted only after that verification.
#
# EXIT CODES
#   0   the PR merged, the landed tree matched, and the remote branch is gone
#   1   validation, certification, merge, verification, or cleanup failed
#   2   merge-preflight found a textual conflict

set -uo pipefail

die() {
    printf 'land-pr: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'land-pr: %s\n' "$1" >&2
}

readonly USAGE="usage: land-pr.sh <pr>"

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"
script="$root/scripts/land-pr.sh"

require_merge_lock() {
    case ":${STORYHOOK_MACHINE_LOCKS:-}:" in
    (*:merge:*) ;;
    (*) die "the private landing phase must run under machine-lock.sh merge" ;;
    esac
}

# The deliberately narrow behavioural seam: real refs, the production
# receipt reader, and a real lock, followed by an arbitrary command. The live
# path supplies this script's own GitHub phase as that command. Tests supply a
# filesystem witness, not a fake `gh`, so they prove refusal and ordering
# without making claims about an imitation GitHub API.
if [ "${1:-}" = "--certified-run" ]; then
    require_merge_lock
    [ "$#" -ge 5 ] && [ "${4:-}" = "--" ] \
        || die "private usage: land-pr.sh --certified-run <base-ref> <head-ref> -- <command...>"
    base="$2"
    head="$3"
    shift 4

    tree="$(bash "$root/scripts/merge-preflight.sh" "$base" "$head")"
    status=$?
    [ "$status" -eq 0 ] || exit "$status"

    STORYHOOK_CERTIFIED_MERGE_TREE="$tree"
    export STORYHOOK_CERTIFIED_MERGE_TREE
    note "certified tree $tree inside the merge lock; running the merge command"
    exec "$@"
fi

# Live GitHub mutation. This mode is reachable only from --locked, through the
# certified-command path above, with both the lock proof and exact tree in its
# environment.
if [ "${1:-}" = "--merge" ]; then
    require_merge_lock
    [ "$#" -eq 6 ] \
        || die "private usage: land-pr.sh --merge <number> <base-ref> <head-ref> <head-sha> <base-remote-ref>"
    [ -n "${STORYHOOK_CERTIFIED_MERGE_TREE:-}" ] \
        || die "the merge phase has no certified merge tree"

    number="$2"
    base_ref="$3"
    head_ref="$4"
    head_sha="$5"
    base_remote_ref="$6"
    expected_tree="$STORYHOOK_CERTIFIED_MERGE_TREE"

    note "merging PR #$number at head $head_sha"
    gh pr merge "$number" --merge --match-head-commit "$head_sha" \
        || die "gh did not merge PR #$number"

    merged="$(gh pr view "$number" --json state,mergedAt,mergeCommit 2>/dev/null)" \
        || die "could not verify PR #$number after gh returned success"
    state="$(printf '%s\n' "$merged" | jq -er '.state')" \
        || die "PR #$number verification returned no state"
    merged_at="$(printf '%s\n' "$merged" | jq -er '.mergedAt // empty')" \
        || die "PR #$number verification returned no merge timestamp"
    merge_oid="$(printf '%s\n' "$merged" | jq -er '.mergeCommit.oid // empty')" \
        || die "PR #$number verification returned no merge commit"
    [ "$state" = "MERGED" ] \
        || die "PR #$number is '$state' after gh returned success, not MERGED"

    git fetch -q origin "+refs/heads/$base_ref:$base_remote_ref" \
        || die "could not refresh origin/$base_ref after merging PR #$number"
    git cat-file -e "$merge_oid^{commit}" 2>/dev/null \
        || die "GitHub reports merge commit $merge_oid, but the refreshed base does not contain its object"
    git merge-base --is-ancestor "$merge_oid" "$base_remote_ref" \
        || die "GitHub reports merge commit $merge_oid, but it is not on origin/$base_ref"
    actual_tree="$(git rev-parse "$merge_oid^{tree}" 2>/dev/null)" \
        || die "could not read the tree of landed merge commit $merge_oid"
    [ "$actual_tree" = "$expected_tree" ] \
        || die "HARD FAILURE: PR #$number landed tree $actual_tree, but the certified tree was $expected_tree (SH-474 base race)"

    note "verified PR #$number merged at $merged_at as $merge_oid with certified tree $actual_tree"

    if git ls-remote --exit-code --heads origin "refs/heads/$head_ref" >/dev/null 2>&1; then
        git push -q origin --delete "refs/heads/$head_ref" \
            || die "PR #$number merged and verified, but deleting remote branch $head_ref failed"
    fi
    if git ls-remote --exit-code --heads origin "refs/heads/$head_ref" >/dev/null 2>&1; then
        die "PR #$number merged and verified, but remote branch $head_ref still exists"
    fi

    note "deleted remote source branch $head_ref; local worktree cleanup remains the Storyhook reap step"
    exit 0
fi

# Everything from metadata resolution through verification stays under the
# lock. A metadata/ref disagreement is refused rather than retried silently:
# the caller can rerun, obtaining a fresh, auditable certification attempt.
if [ "${1:-}" = "--locked" ]; then
    require_merge_lock
    [ "$#" -eq 2 ] || die "$USAGE"
    pr="$2"

    command -v gh >/dev/null 2>&1 || die "the gh CLI is required and was not found"
    command -v jq >/dev/null 2>&1 || die "jq is required and was not found"

    initial="$(gh pr view "$pr" --json number,baseRefName 2>/dev/null)" \
        || die "could not read PR '$pr' — is gh authenticated, and does the PR exist?"
    number="$(printf '%s\n' "$initial" | jq -er '.number')" \
        || die "PR '$pr' returned no number"
    base_ref="$(printf '%s\n' "$initial" | jq -er '.baseRefName')" \
        || die "PR #$number returned no base branch"

    base_remote_ref="refs/remotes/origin/$base_ref"
    head_remote_ref="refs/remotes/origin/pr/$number"
    note "refreshing origin/$base_ref and PR #$number under the merge lock"
    git fetch -q origin \
        "+refs/heads/$base_ref:$base_remote_ref" \
        "+refs/pull/$number/head:$head_remote_ref" \
        || die "could not fetch the base and head refs for PR #$number"

    current="$(gh pr view "$number" --json state,isDraft,isCrossRepository,baseRefName,baseRefOid,headRefName,headRefOid 2>/dev/null)" \
        || die "could not refresh metadata for PR #$number"
    state="$(printf '%s\n' "$current" | jq -er '.state')" || die "PR #$number returned no state"
    draft="$(printf '%s\n' "$current" | jq -r '.isDraft')" || die "PR #$number returned no draft status"
    cross="$(printf '%s\n' "$current" | jq -r '.isCrossRepository')" || die "PR #$number returned no repository relationship"
    current_base_ref="$(printf '%s\n' "$current" | jq -er '.baseRefName')" || die "PR #$number returned no base branch"
    base_sha="$(printf '%s\n' "$current" | jq -er '.baseRefOid')" || die "PR #$number returned no base SHA"
    head_ref="$(printf '%s\n' "$current" | jq -er '.headRefName')" || die "PR #$number returned no head branch"
    head_sha="$(printf '%s\n' "$current" | jq -er '.headRefOid')" || die "PR #$number returned no head SHA"

    [ "$state" = "OPEN" ] || die "PR #$number is '$state', not OPEN"
    [ "$draft" = "false" ] || die "PR #$number is a draft or returned an invalid draft status '$draft'"
    [ "$cross" = "false" ] \
        || die "PR #$number comes from a fork; autonomous lanes only land same-repository branches"
    [ "$current_base_ref" = "$base_ref" ] \
        || die "PR #$number changed base branches while its refs were being refreshed"

    fetched_base="$(git rev-parse "$base_remote_ref" 2>/dev/null)" \
        || die "could not resolve fetched base ref $base_remote_ref"
    fetched_head="$(git rev-parse "$head_remote_ref" 2>/dev/null)" \
        || die "could not resolve fetched head ref $head_remote_ref"
    [ "$fetched_base" = "$base_sha" ] \
        || die "PR #$number base moved while it was being refreshed ($fetched_base fetched, $base_sha reported); rerun land-pr.sh"
    [ "$fetched_head" = "$head_sha" ] \
        || die "PR #$number head moved while it was being refreshed ($fetched_head fetched, $head_sha reported); rerun land-pr.sh"

    exec bash "$script" --certified-run "$base_sha" "$head_sha" -- \
        bash "$script" --merge "$number" "$base_ref" "$head_ref" "$head_sha" "$base_remote_ref"
fi

[ "$#" -eq 1 ] || die "$USAGE"
pr="$1"
case "$pr" in
('' | -*) die "$USAGE" ;;
esac

exec bash "$root/scripts/machine-lock.sh" merge -- bash "$script" --locked "$pr"
