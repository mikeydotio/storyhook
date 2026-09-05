#!/usr/bin/env bash
#
# Verify and land exactly one StoryHook-submitted pull request (SH-521).
#
# Queue selection belongs to the daemon. This script owns the repository-side
# transaction for that one candidate: refresh the base and PR refs, compute the
# exact merge tree, run the release gate in the persistent verifier worktree
# when needed, then delegate the guarded merge to land-pr.sh.

set -uo pipefail

die_json() {
    jq -n --arg detail "$1" '{result:"infrastructure-failure", detail:$detail}'
    exit 0
}

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die_json "not inside a git worktree"
cd "$root" || die_json "cannot enter repository root $root"
# Progress emission degrades to a no-op rather than a source failure: this
# script runs against WHATEVER checkout the daemon has registered
# (`current_dir(&candidate.checkout)`), and a disposable test fixture is a
# real git worktree with no `scripts/` tree of its own copied into it.
if [ -f "$root/scripts/gate-progress.sh" ]; then
    # shellcheck source=gate-progress.sh
    . "$root/scripts/gate-progress.sh"
else
    gate_progress_emit_item() { :; }
    gate_progress_emit_case() { :; }
fi
# SH-545: the verifier tmux mirror. Same source-if-present shape as
# gate-progress.sh above, and the same degrade-silently posture: a mirror
# failure (missing tmux, a disposable test fixture with no scripts/ tree,
# STORYHOOK_VERIFIER_MIRROR=0) must never affect verification's own result.
if [ -f "$root/scripts/verify-window.sh" ]; then
    # shellcheck source=verify-window.sh
    . "$root/scripts/verify-window.sh"
else
    verifier_window_banner() { :; }
    verifier_window_tail() { :; }
fi
command -v jq >/dev/null 2>&1 || die_json "jq is required"
common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd -P)" \
    || die_json "could not resolve the shared git directory"
verifier_wt="$common_dir/storyhook/verification-worktree"
verifier_format="$common_dir/storyhook/verification-worktree.format"
readonly VERIFIER_FORMAT_VERSION="private-gitdir-v1"

ensure_verifier_worktree() {
    fallback="$1"
    git cat-file -e "$fallback^{commit}" 2>/dev/null \
        || die_json "cannot repair the verifier worktree from unavailable commit $fallback"
    mkdir -p "$(dirname "$verifier_wt")" \
        || die_json "could not create private verifier state"

    rebuild=0
    if [ -e "$verifier_wt/.git" ]; then
        [ -f "$verifier_wt/.git" ] && [ ! -L "$verifier_wt/.git" ] \
            || die_json "the verifier worktree has an invalid .git entry at $verifier_wt/.git"
        installed_format="$(cat "$verifier_format" 2>/dev/null || true)"
        if [ "$installed_format" != "$VERIFIER_FORMAT_VERSION" ]; then
            rebuild=1
        elif ! git -C "$verifier_wt" cat-file -e 'HEAD^{commit}' 2>/dev/null \
            || ! git -C "$verifier_wt" reflog show --format='%H' HEAD >/dev/null 2>&1; then
            rebuild=1
        fi
    elif [ -e "$verifier_wt" ]; then
        die_json "the verifier path $verifier_wt exists without a registered worktree; refusing to overwrite unclassified evidence"
    fi

    if [ "$rebuild" -eq 1 ]; then
        git worktree remove --force "$verifier_wt" >/dev/null 2>&1 \
            || die_json "could not remove invalid verifier worktree metadata at $verifier_wt"
    fi
    if [ ! -e "$verifier_wt/.git" ]; then
        git worktree add -q --detach "$verifier_wt" "$fallback" \
            || die_json "could not create the persistent verifier worktree at $fallback"
    fi

    marker_tmp="$(mktemp "$verifier_format.XXXXXX")" \
        || die_json "could not stage the verifier worktree format marker"
    if ! printf '%s\n' "$VERIFIER_FORMAT_VERSION" > "$marker_tmp" \
        || ! mv -f "$marker_tmp" "$verifier_format"; then
        rm -f "$marker_tmp"
        die_json "could not record the verifier worktree format"
    fi
}

classify_land() {
    land_status="$1"
    land_output="$2"
    landed_pr="$3"
    landed_tree="$4"
    case "$land_status" in
    (0)
        jq -n --arg tree "$landed_tree" --arg detail "$land_output" \
            '{result:"merged", tree:$tree, detail:$detail}'
        ;;
    (2)
        jq -n --arg detail "$land_output" '{result:"conflict", detail:$detail}'
        ;;
    (*)
        die_json "land-pr.sh refused PR #$landed_pr after verification: $land_output"
        ;;
    esac
    exit 0
}

recover_merged() {
    recovered_base="$1"
    merge_oid="$2"
    recovered_pr="$3"
    git cat-file -e "$merge_oid^{commit}" 2>/dev/null \
        || die_json "merged PR #$recovered_pr reports $merge_oid, but the refreshed base does not carry its object"
    git merge-base --is-ancestor "$merge_oid" "$recovered_base" \
        || die_json "merged PR #$recovered_pr reports $merge_oid, but it is not on the refreshed base"
    tree="$(git rev-parse "$merge_oid^{tree}" 2>/dev/null)" \
        || die_json "could not read merged PR #$recovered_pr tree"
    receipt="$common_dir/storyhook/gate-receipts/$tree"
    [ -f "$receipt" ] \
        || die_json "merged PR #$recovered_pr landed tree $tree without a release-gate receipt"
    tier="$(sed -n 's/^tier //p' "$receipt" | head -n1)"
    tier="${tier:-gate}"
    case "$tier" in
    gate | full) ;;
    *) die_json "merged PR #$recovered_pr landed tree $tree with insufficient '$tier' receipt" ;;
    esac
    jq -n --arg tree "$tree" --arg detail "recovered already-merged PR #$recovered_pr at $merge_oid after verifier restart" \
        '{result:"merged", tree:$tree, detail:$detail}'
    exit 0
}

validate_metadata() {
    metadata="$1"
    pr="$(printf '%s' "$metadata" | jq -er '.number')" \
        || die_json "submitted pull request returned no number"
    state="$(printf '%s' "$metadata" | jq -er '.state')" \
        || die_json "PR #$pr returned no state"

    # `jq -e` assigns failure to the JSON value `false`, so it cannot extract
    # boolean fields whose healthy value is false. Select the type as data,
    # then keep the business-policy checks below separate from wire validity.
    draft="$(printf '%s' "$metadata" | jq -r \
        'if ((.isDraft | type) == "boolean") then .isDraft else empty end')" \
        || die_json "PR #$pr returned no draft status"
    [ -n "$draft" ] || die_json "PR #$pr returned no draft status"
    cross="$(printf '%s' "$metadata" | jq -r \
        'if ((.isCrossRepository | type) == "boolean") then .isCrossRepository else empty end')" \
        || die_json "PR #$pr returned no repository relationship"
    [ -n "$cross" ] || die_json "PR #$pr returned no repository relationship"

    base="$(printf '%s' "$metadata" | jq -er '.baseRefName')" \
        || die_json "PR #$pr returned no base branch"
    reported_head="$(printf '%s' "$metadata" | jq -er '.headRefOid')" \
        || die_json "PR #$pr returned no head oid"
    [ "$draft" = false ] || die_json "PR #$pr is a draft"
    [ "$cross" = false ] \
        || die_json "PR #$pr comes from a fork; centralized verification accepts same-repository PRs only"
}

# Metadata-validation seam. The production path supplies GitHub's JSON; tests
# feed that same wire shape directly so boolean parsing is proven without a
# fake GitHub service or any repository mutation.
if [ "${1:-}" = --validate-metadata ]; then
    [ "$#" -eq 2 ] \
        || die_json "private usage: verify-pr.sh --validate-metadata <json>"
    validate_metadata "$2"
    jq -n --argjson number "$pr" '{result:"metadata-valid", number:$number}'
    exit 0
fi

# Protocol-classification seam. The live path supplies land-pr.sh's real
# status and diagnostics; tests can pin the existing status contract without
# imitating GitHub.
if [ "${1:-}" = --classify-land ]; then
    [ "$#" -eq 5 ] \
        || die_json "private usage: verify-pr.sh --classify-land <status> <detail> <pr-number> <tree>"
    classify_land "$2" "$3" "$4" "$5"
fi

# Real-Git recovery seam. The public path refreshes GitHub's base before
# entering it; tests can exercise the local proof without imitating GitHub.
if [ "${1:-}" = --recover-merged ]; then
    [ "$#" -eq 4 ] \
        || die_json "private usage: verify-pr.sh --recover-merged <base-ref> <merge-oid> <pr-number>"
    recover_merged "$2" "$3" "$4"
fi

# Real-Git verifier repair seam. Production enters the same function before
# its first fetch; tests can strand private worktree objects and prove the
# recovery without a GitHub imitation.
if [ "${1:-}" = --ensure-verifier-worktree ]; then
    [ "$#" -eq 2 ] \
        || die_json "private usage: verify-pr.sh --ensure-verifier-worktree <fallback-commit>"
    ensure_verifier_worktree "$2"
    jq -n --arg worktree "$verifier_wt" \
        '{result:"verifier-worktree-ready", worktree:$worktree}'
    exit 0
fi

[ "$#" -eq 1 ] || die_json "usage: verify-pr.sh <pr-url>"
submitted_pr="$1"
command -v gh >/dev/null 2>&1 || die_json "the gh CLI is required"

verifier_window_banner "verifying $submitted_pr — checking pull request metadata"
gate_progress_emit_item "pull request metadata" running
_pr_meta_start=$(date +%s)
metadata="$(gh pr view "$submitted_pr" --json number,state,isDraft,isCrossRepository,baseRefName,headRefOid,mergeCommit 2>/dev/null)" \
    || die_json "could not read submitted pull request $submitted_pr from GitHub"
validate_metadata "$metadata"
gate_progress_emit_item "pull request metadata" passed "seconds=$(( $(date +%s) - _pr_meta_start ))"

base_ref="refs/remotes/origin/$base"
head_ref="refs/remotes/origin/pr/$pr"
fallback="$(git rev-parse 'HEAD^{commit}' 2>/dev/null)" \
    || die_json "could not resolve a local commit for verifier worktree recovery"
ensure_verifier_worktree "$fallback"
if [ "$state" = MERGED ]; then
    merge_oid="$(printf '%s' "$metadata" | jq -er '.mergeCommit.oid // empty')" \
        || die_json "merged PR #$pr returned no merge commit"
    git fetch -q origin "+refs/heads/$base:$base_ref" \
        || die_json "could not refresh origin/$base for merged PR #$pr"
    recover_merged "$base_ref" "$merge_oid" "$pr"
fi

[ "$state" = OPEN ] || die_json "PR #$pr is $state, not OPEN or MERGED"
git fetch -q origin \
    "+refs/heads/$base:$base_ref" \
    "+refs/pull/$pr/head:$head_ref" \
    || die_json "could not refresh origin/$base and PR #$pr"
head="$(git rev-parse "$head_ref" 2>/dev/null)" || die_json "could not resolve fetched PR #$pr"
[ "$head" = "$reported_head" ] || die_json "PR #$pr moved while its refs were being refreshed"

verifier_window_banner "PR #$pr — merge preflight running (computing the exact merge tree)"
gate_progress_emit_item "merge preflight" running
_preflight_start=$(date +%s)
preflight="$(bash scripts/merge-preflight.sh "$base_ref" "$head_ref" 2>&1)"
preflight_status=$?
tree="$(printf '%s\n' "$preflight" | head -n1)"
_preflight_seconds=$(( $(date +%s) - _preflight_start ))
case "$preflight_status" in
(2)
    gate_progress_emit_item "merge preflight" failed "seconds=$_preflight_seconds"
    jq -n --arg detail "$preflight" '{result:"conflict", detail:$detail}'
    exit 0
    ;;
(0)
    gate_progress_emit_item "merge preflight" passed "seconds=$_preflight_seconds"
    gate_progress_emit_item "release gate" reused
    verifier_window_banner "PR #$pr — merge tree $tree already certified; release gate reused, no live make-test output for this run"
    ;;
(1)
    gate_progress_emit_item "merge preflight" passed "seconds=$_preflight_seconds"
    logs="$common_dir/storyhook/verification-logs"
    mkdir -p "$logs" || die_json "could not create verification log directory"
    log="$logs/pr-$pr-$tree.log"
    verifier_window_tail "$log"
    if ! bash scripts/merge-watch.sh --speculative-run "$tree" \
        "$base_ref" "$head_ref" "$verifier_wt" -- make test >"$log" 2>&1; then
        tail_context="$(tail -n 40 "$log")"
        jq -n --arg tree "$tree" --arg log "$log" --arg detail "$tail_context" \
            '{result:"tests-failed", tree:$tree, log:$log, detail:$detail}'
        exit 0
    fi
    ;;
(*)
    gate_progress_emit_item "merge preflight" failed "seconds=$_preflight_seconds"
    die_json "merge preflight returned unexpected status $preflight_status: $preflight"
    ;;
esac

verifier_window_banner "PR #$pr — merge tree $tree passed; landing pull request"
gate_progress_emit_item "land pull request" running
_land_start=$(date +%s)
land_output="$(bash scripts/land-pr.sh "$submitted_pr" 2>&1)"
land_status=$?
gate_progress_emit_item "land pull request" \
    "$([ "$land_status" = 0 ] && echo passed || echo failed)" \
    "seconds=$(( $(date +%s) - _land_start ))"
classify_land "$land_status" "$land_output" "$pr" "$tree"
