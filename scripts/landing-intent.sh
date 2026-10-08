#!/usr/bin/env bash
# The daemon commits durable authority before invoking this phase (SH-656).
set -uo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 1
# shellcheck source=github-access.sh
. "$script_dir/github-access.sh" || exit 1
managed_owner="" managed_attempt=""
if [ "${1:-}" = --managed-integration ]; then
    [ "$#" -eq 8 ] || exit 1
    managed_owner="$2" managed_attempt="$3"
    uuid_pattern='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    [[ "$managed_owner" =~ $uuid_pattern ]] && [[ "$managed_attempt" =~ $uuid_pattern ]] || exit 1
    shift 3
fi
[ "$#" -eq 5 ] || [ "$#" -eq 6 ] || exit 1
mode="$1" pr="$2" expected_head="$3" expected_tree="$4" marker="$5"
skipped_attempt="${6:-}"
if [ -n "$managed_owner" ]; then
    [[ "$expected_head" =~ ^[0-9a-f]{40}$ ]] && [[ "$expected_tree" =~ ^[0-9a-f]{40}$ ]] || exit 1
    [ "${marker##*/}" = "landing-$managed_attempt.attempted" ] || exit 1
    case "$pr" in ''|-*) exit 1 ;; esac
fi
verdict() {
    jq -n --arg result "$1" --arg detail "$2" '{result:$result, detail:$detail}'
    exit 0
}
case "$mode" in attempt|recover) ;; *) exit 1 ;; esac
# Refusal before an attempt is distinct from an uncertain recovery observation.
# shellcheck source=python-runtime.sh
. "$script_dir/python-runtime.sh" || verdict uncertain "missing Python runtime policy in $script_dir"
if ! storyhook_python_init; then
    if [ "$mode" = attempt ] && [ ! -e "$marker" ]; then
        verdict not-attempted "$STORYHOOK_PYTHON_ERROR"
    fi
    verdict uncertain "$STORYHOOK_PYTHON_ERROR"
fi
github_begin || verdict uncertain "cannot establish GitHub origin: ${GITHUB_ACCESS_ERROR:-origin unavailable}"
# A recovery never retries a mutation. OPEN does not prove an earlier request
# cannot finish, even when a new daemon cannot find the old local marker.
not_attempted=""
output="recovery observation only"
if [ "$mode" = attempt ]; then
    [ ! -e "$marker" ] || verdict uncertain "this intent has already attempted a merge"
    export STORYHOOK_LANDING_HEAD="$expected_head" STORYHOOK_LANDING_TREE="$expected_tree"
    export STORYHOOK_LANDING_ATTEMPT_MARKER="$marker"
    landing_args=("$pr")
    if [ -n "$managed_owner" ]; then
        landing_args=(--managed-intent "$managed_owner" "$managed_attempt" "$expected_head" "$expected_tree" "$pr")
    elif [ -n "$skipped_attempt" ]; then
        landing_args=(--stopped-intent "$skipped_attempt" "$expected_head" "$expected_tree" "$pr")
    fi
    output="$(bash "$script_dir/land-pr.sh" "${landing_args[@]}" 2>&1)"
    status=$?
    if [ "$status" -ne 0 ] && [ ! -e "$marker" ]; then
        not_attempted="$output"
    fi
fi
metadata_fields=state,headRefOid,baseRefName,mergeCommit
if [ -n "$managed_owner" ]; then metadata_fields+=,headRefName; fi
metadata="$(github_exec pr view "$pr" --json "$metadata_fields" 2>&1)" \
    || verdict uncertain "cannot observe admitted pull request: $metadata"
if [ -n "$managed_owner" ]; then
    observed_branch="$(printf '%s\n' "$metadata" | jq -er '.headRefName')" || verdict uncertain "missing managed head branch"
    [ "$observed_branch" = "storyhook/integration/$managed_owner" ] || verdict uncertain "managed pull request head branch differs from its owner"
fi
state="$(printf '%s\n' "$metadata" | jq -er '.state')" || verdict uncertain "missing PR state"
if [ "$state" != MERGED ]; then
    if [ -n "$not_attempted" ]; then verdict not-attempted "$not_attempted"; fi
    # A receipt belongs to this exact marker, admitted head/tree and PR. A
    # missing/torn/mismatched receipt never relaxes the uncertain-outcome fence.
    number="${pr##*/}"
    if { [ "$state" = OPEN ] || [ "$state" = CLOSED ]; } && \
        [ "$(cat "$marker" 2>/dev/null)" = "$expected_head $expected_tree" ] && \
        jq -e --arg head "$expected_head" --arg tree "$expected_tree" --arg number "$number" \
        '.version == 1 and .head == $head and .tree == $tree and .number == $number and
         (.status | type == "number") and .status == (.status | floor) and
         .status >= 400 and .status < 500 and .status != 408' \
        "$marker.refused" >/dev/null 2>&1; then
        status="$(jq -r '.status' "$marker.refused")"
        verdict refused "GitHub synchronously refused the admitted merge with HTTP $status; observed pull request is $state"
    fi
    verdict uncertain "admitted pull request is $state; an earlier request may still complete. $output"
fi
head="$(printf '%s\n' "$metadata" | jq -er '.headRefOid')" || verdict uncertain "missing merged head"
[ "$head" = "$expected_head" ] || verdict uncertain "merged head differs from admitted head"
base="$(printf '%s\n' "$metadata" | jq -er '.baseRefName')" || verdict uncertain "missing merged base"
merge_oid="$(printf '%s\n' "$metadata" | jq -er '.mergeCommit.oid')" || verdict uncertain "missing merge commit"
git check-ref-format "refs/heads/$base" >/dev/null || verdict uncertain "invalid merge base ref"
# Fetch to a private ref so another verifier's fetch cannot change this check.
recovery_ref="refs/storyhook/landing/$expected_head"
github_git fetch -q origin "+refs/heads/$base:$recovery_ref" 2>&1 \
    || verdict uncertain "cannot fetch merged base"
git merge-base --is-ancestor "$merge_oid" "$recovery_ref" \
    || verdict uncertain "reported merge is not on the fetched base"
actual_tree="$(git rev-parse "$merge_oid^{tree}" 2>/dev/null)" \
    || verdict uncertain "cannot resolve landed tree"
[ "$actual_tree" = "$expected_tree" ] || verdict uncertain "landed tree differs from admitted tree"
verdict merged "confirmed admitted head $expected_head landed as $merge_oid with admitted tree $actual_tree"
