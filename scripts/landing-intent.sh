#!/usr/bin/env bash
# The daemon commits durable authority before invoking this phase (SH-656).
set -uo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 1
[ "$#" -eq 5 ] || exit 1
mode="$1" pr="$2" expected_head="$3" expected_tree="$4" marker="$5"
verdict() {
    jq -n --arg result "$1" --arg detail "$2" '{result:$result, detail:$detail}'
    exit 0
}
case "$mode" in attempt|recover) ;; *) exit 1 ;; esac
# A recovery never retries a mutation. OPEN does not prove an earlier request
# cannot finish, even when a new daemon cannot find the old local marker.
not_attempted=""
output="recovery observation only"
if [ "$mode" = attempt ]; then
    [ ! -e "$marker" ] || verdict uncertain "this intent has already attempted a merge"
    export STORYHOOK_LANDING_HEAD="$expected_head" STORYHOOK_LANDING_TREE="$expected_tree"
    export STORYHOOK_LANDING_ATTEMPT_MARKER="$marker"
    output="$(bash "$script_dir/land-pr.sh" "$pr" 2>&1)"
    status=$?
    if [ "$status" -ne 0 ] && [ ! -e "$marker" ]; then
        not_attempted="$output"
    fi
fi
metadata="$(gh pr view "$pr" --json state,headRefOid,baseRefName,mergeCommit 2>&1)" \
    || verdict uncertain "cannot observe admitted pull request: $metadata"
state="$(printf '%s\n' "$metadata" | jq -er '.state')" || verdict uncertain "missing PR state"
if [ "$state" != MERGED ]; then
    if [ -n "$not_attempted" ]; then verdict not-attempted "$not_attempted"; fi
    verdict uncertain "admitted pull request is $state; an earlier request may still complete. $output"
fi
head="$(printf '%s\n' "$metadata" | jq -er '.headRefOid')" || verdict uncertain "missing merged head"
[ "$head" = "$expected_head" ] || verdict uncertain "merged head differs from admitted head"
base="$(printf '%s\n' "$metadata" | jq -er '.baseRefName')" || verdict uncertain "missing merged base"
merge_oid="$(printf '%s\n' "$metadata" | jq -er '.mergeCommit.oid')" || verdict uncertain "missing merge commit"
git check-ref-format "refs/heads/$base" >/dev/null || verdict uncertain "invalid merge base ref"
# Fetch to a private ref so another verifier's fetch cannot change this check.
recovery_ref="refs/storyhook/landing/$expected_head"
git fetch -q origin "+refs/heads/$base:$recovery_ref" 2>&1 \
    || verdict uncertain "cannot fetch merged base"
git merge-base --is-ancestor "$merge_oid" "$recovery_ref" \
    || verdict uncertain "reported merge is not on the fetched base"
actual_tree="$(git rev-parse "$merge_oid^{tree}" 2>/dev/null)" \
    || verdict uncertain "cannot resolve landed tree"
[ "$actual_tree" = "$expected_tree" ] || verdict uncertain "landed tree differs from certified tree"
verdict merged "confirmed admitted head $expected_head landed as $merge_oid with certified tree $actual_tree"
