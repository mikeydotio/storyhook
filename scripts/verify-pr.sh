#!/usr/bin/env bash
#
# Verify and land exactly one StoryHook-submitted pull request (SH-521).
#
# Queue selection belongs to the daemon. This script owns the repository-side
# transaction for that one candidate: refresh the base and PR refs, compute the
# exact merge tree, run the release gate in the persistent verifier worktree
# when needed, then delegate the guarded merge to land-pr.sh.

set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 1
# shellcheck source=activity-log.sh
. "$script_dir/activity-log.sh"

die_json() {
    jq -n --arg detail "$1" \
        '{result:"infrastructure-failure", disposition:"permanent", detail:$detail}'
    exit 0
}

retry_json() {
    jq -n --arg detail "$1" \
        '{result:"infrastructure-failure", disposition:"retryable", detail:$detail}'
    exit 0
}

invalid_json() {
    jq -n --arg detail "$1" '{result:"invalid-submission", detail:$detail}'
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
        remove_output="$(git worktree remove --force "$verifier_wt" 2>&1)"
        remove_status=$?
        registration_survives=0
        git worktree list --porcelain 2>/dev/null \
            | awk -v target="$verifier_wt" \
                '$1 == "worktree" && substr($0, 10) == target { found = 1 } END { exit !found }' \
            && registration_survives=1
        if [ -e "$verifier_wt" ] || [ "$registration_survives" -eq 1 ]; then
            remove_detail="${remove_output:-git worktree remove exited $remove_status without a diagnostic}"
            die_json "could not remove invalid verifier worktree metadata at $verifier_wt: $remove_detail"
        fi
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

bounded_log_context() {
    context_log="$1"
    context_lines="$(awk 'END { print NR + 0 }' "$context_log")"
    if [ "$context_lines" -gt 40 ]; then
        printf 'Ancillary context — last 40 of %s log lines (%s earlier lines omitted):\n' \
            "$context_lines" "$((context_lines - 40))"
    else
        printf 'Ancillary context — all %s log lines (no truncation):\n' "$context_lines"
    fi
    tail -n 40 "$context_log" | awk '
        length($0) > 500 {
            print substr($0, 1, 500) " … [line truncated at 500 characters]"
            next
        }
        { print }
    '
}

verification_failure_detail() {
    failure_status="$1"
    failure_log="$2"

    raw_failed="$(python3 "$script_dir/test_output.py" <"$failure_log" \
        | awk -F '\t' '$3 == "FAIL" { print $1 "::" $2 }')"
    delta_failed="$(awk '
        /^test-delta: (newly RED|still red) \([0-9]+\):$/ {
            capture = 1
            next
        }
        capture && /^  / {
            line = $0
            sub(/^  /, "", line)
            print line
            next
        }
        { capture = 0 }
    ' "$failure_log")"
    failed_tests="$(printf '%s\n%s\n' "$raw_failed" "$delta_failed" \
        | awk 'NF && !seen[$0]++')"
    failed_count="$(printf '%s\n' "$failed_tests" \
        | awk 'NF { count += 1 } END { print count + 0 }')"

    compiler_diagnostics="$(awk '
        /^error(\[[^]]+\])?: / || /^error: / {
            if ($0 !~ /^error: (test|doctest) failed/) print
        }
    ' "$failure_log" | awk 'NF && !seen[$0]++')"
    compiler_count="$(printf '%s\n' "$compiler_diagnostics" \
        | awk 'NF { count += 1 } END { print count + 0 }')"

    reused_count="$(awk '/^leg [^:]+: REUSED / { count += 1 } END { print count + 0 }' \
        "$failure_log")"
    not_rerun_count="$(awk '
        /^test-delta: not re-run since .* \([0-9]+\):$/ {
            line = $0
            sub(/^.*\(/, "", line)
            sub(/\):$/, "", line)
            count += line
        }
        END { print count + 0 }
    ' "$failure_log")"

    printf 'Verification failure summary\n'
    printf 'The completed gate failed with exit status %s.\n' "$failure_status"
    if [ "$failed_count" -gt 0 ]; then
        if [ "$failed_count" -gt 20 ]; then
            printf 'Failed tests (%s; showing first 20):\n' "$failed_count"
        else
            printf 'Failed tests (%s):\n' "$failed_count"
        fi
        printf '%s\n' "$failed_tests" | sed -n '1,20p' | awk '
            {
                if (length($0) > 500) {
                    print "  - " substr($0, 1, 500) " … [line truncated at 500 characters]"
                } else {
                    print "  - " $0
                }
            }
        '
        if [ "$failed_count" -gt 20 ]; then
            printf '  … %s additional failed tests omitted; see full log.\n' \
                "$((failed_count - 20))"
        fi
    fi
    if [ "$compiler_count" -gt 0 ]; then
        if [ "$compiler_count" -gt 10 ]; then
            printf 'Compiler/build diagnostics (%s; showing first 10):\n' "$compiler_count"
        else
            printf 'Compiler/build diagnostics (%s):\n' "$compiler_count"
        fi
        printf '%s\n' "$compiler_diagnostics" | sed -n '1,10p' | awk '
            {
                if (length($0) > 500) {
                    print "  - " substr($0, 1, 500) " … [line truncated at 500 characters]"
                } else {
                    print "  - " $0
                }
            }
        '
        if [ "$compiler_count" -gt 10 ]; then
            printf '  … %s additional compiler/build diagnostics omitted; see full log.\n' \
                "$((compiler_count - 10))"
        fi
    fi
    if [ "$failed_count" -eq 0 ] && [ "$compiler_count" -eq 0 ]; then
        printf 'No failed test or compiler/build diagnostic was recognized; inspect the full log for the cause.\n'
    fi
    if [ "$reused_count" -gt 0 ]; then
        printf 'Reused/cached legs (%s): these successful results were reused; they are not failures.\n' \
            "$reused_count"
    fi
    if [ "$not_rerun_count" -gt 0 ]; then
        printf 'Not re-run (%s): status unknown; not counted as pass or failure.\n' \
            "$not_rerun_count"
    fi
    printf '\n'
    bounded_log_context "$failure_log"
}

verification_infrastructure_detail() {
    infrastructure_status="$1"
    infrastructure_completed_status="$2"
    infrastructure_log="$3"

    printf 'Verification infrastructure failure\n'
    printf 'Gate process exit status: %s.\n' "$infrastructure_status"
    if [ -z "$infrastructure_completed_status" ]; then
        printf 'Completion record: missing.\n'
    else
        printf 'Completion record: exit status %s, which does not match the gate process.\n' \
            "$infrastructure_completed_status"
    fi
    printf 'Candidate test status: unknown — the gate did not complete with successful restoration, so this is not classified as a test failure.\n\n'
    bounded_log_context "$infrastructure_log"
    printf 'Verification log: %s\n' "$infrastructure_log"
}

run_verification_gate() {
    gate_pr="$1"
    gate_tree="$2"
    gate_base="$3"
    gate_head="$4"
    gate_worktree="$5"
    shift 5
    logs="$common_dir/storyhook/verification-logs"
    mkdir -p "$logs" || die_json "could not create verification log directory"
    log="$(mktemp "$logs/pr-$gate_pr-$gate_tree-attempt.XXXXXX")" \
        || die_json "could not create per-attempt verification log"
    gate_result="$(mktemp "$logs/pr-$gate_pr-result.XXXXXX")" \
        || die_json "could not create gate completion record"
    verifier_window_tail "$log"
    STORYHOOK_GATE_PROGRESS_ACTIVITY_PATH="release gate" \
        STORYHOOK_GATE_RESULT_FILE="$gate_result" \
        activity_run "machine-lock.sh/merge-watch.sh" bash "$script_dir/machine-lock.sh" gate -- \
        bash "$script_dir/merge-watch.sh" --speculative-run "$gate_tree" \
        "$gate_base" "$gate_head" "$gate_worktree" -- "$@" >"$log" 2>&1
    gate_status=$?
    completed_status="$(cat "$gate_result")" || completed_status=""
    rm -f "$gate_result" || die_json "could not remove gate completion record $gate_result"
    # Only a completed gate with successful restoration can blame tests.
    # Preparation failures, signals and cleanup failures leave no record.
    if [ "$completed_status" != "$gate_status" ]; then
        detail="$(verification_infrastructure_detail "$gate_status" "$completed_status" "$log")"
        jq -n --arg tree "$gate_tree" --arg log "$log" --arg detail "$detail" \
            '{result:"infrastructure-failure", disposition:"permanent", tree:$tree, log:$log, detail:$detail}'
        exit 0
    fi
    if [ "$gate_status" -ne 0 ]; then
        detail="$(verification_failure_detail "$gate_status" "$log")"
        jq -n --arg tree "$gate_tree" --arg log "$log" --arg detail "$detail" \
            '{result:"tests-failed", tree:$tree, log:$log, detail:$detail}'
        exit 0
    fi
}

classify_land() {
    land_status="$1"
    land_output="$2"
    landed_pr="$3"
    landed_tree="$4"
    landed_base="$5"
    landed_head="$6"
    case "$land_status" in
    (0)
        jq -n --arg tree "$landed_tree" --arg detail "$land_output" \
            '{result:"merged", tree:$tree, detail:$detail}'
        ;;
    (2)
        jq -n --arg detail "$land_output" '{result:"conflict", detail:$detail}'
        ;;
    (*)
        refreshed_metadata="$(gh pr view "$landed_pr" --json number,state,isDraft,isCrossRepository,baseRefName,headRefOid,mergeCommit 2>/dev/null)"
        refresh_status=$?
        reconcile_land_refusal "$refresh_status" "$refreshed_metadata" \
            "$landed_pr" "$landed_base" "$landed_head" "$landed_tree" "$land_output"
        ;;
    esac
    exit 0
}

recover_merged() {
    recovered_base="$1"
    merge_oid="$2"
    recovered_pr="$3"
    recovery_context="$4"
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
    if [ "$recovery_context" = "after landing refusal" ]; then
        gate_progress_emit_item "land pull request" passed
    fi
    jq -n --arg tree "$tree" --arg detail "recovered already-merged PR #$recovered_pr at $merge_oid $recovery_context" \
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
    [ "$draft" = false ] || invalid_json "PR #$pr is a draft"
    [ "$cross" = false ] \
        || invalid_json "PR #$pr comes from a fork; centralized verification accepts same-repository PRs only"
}

reconcile_land_refusal() {
    refresh_status="$1"
    refreshed_metadata="$2"
    expected_pr="$3"
    expected_base="$4"
    expected_head="$5"
    verified_tree="$6"
    land_detail="$7"

    [ "$refresh_status" -eq 0 ] \
        || retry_json "could not refresh PR #$expected_pr after landing refusal: $land_detail"
    validate_metadata "$refreshed_metadata"
    [ "$pr" = "$expected_pr" ] \
        || invalid_json "landing refresh returned PR #$pr for submitted PR #$expected_pr"
    case "$state" in
    OPEN | MERGED) ;;
    *) invalid_json "PR #$pr is $state after landing refusal, not OPEN or MERGED" ;;
    esac
    [ "$base" = "$expected_base" ] \
        || invalid_json "PR #$pr changed base branch from $expected_base to $base after verification"
    [ "$reported_head" = "$expected_head" ] \
        || invalid_json "PR #$pr changed head from $expected_head to $reported_head after verification"

    base_ref="refs/remotes/origin/$base"
    head_ref="refs/remotes/origin/pr/$pr"
    if [ "$state" = MERGED ]; then
        merge_oid="$(printf '%s' "$refreshed_metadata" | jq -er '.mergeCommit.oid // empty')" \
            || die_json "merged PR #$pr returned no merge commit after landing refusal"
        git fetch -q origin "+refs/heads/$base:$base_ref" \
            || retry_json "could not refresh origin/$base for merged PR #$pr after landing refusal"
        recover_merged "$base_ref" "$merge_oid" "$pr" "after landing refusal"
    fi

    git fetch -q origin \
        "+refs/heads/$base:$base_ref" \
        "+refs/pull/$pr/head:$head_ref" \
        || retry_json "could not refresh origin/$base and PR #$pr after landing refusal"
    refreshed_head="$(git rev-parse "$head_ref" 2>/dev/null)" \
        || die_json "could not resolve refreshed PR #$pr after landing refusal"
    [ "$refreshed_head" = "$expected_head" ] \
        || invalid_json "PR #$pr head moved from $expected_head to $refreshed_head while landing was reconciled"

    refreshed_preflight="$(activity_run "merge-preflight.sh" bash "$script_dir/merge-preflight.sh" "$base_ref" "$head_ref" 2>&1)"
    refreshed_status=$?
    refreshed_tree="$(printf '%s\n' "$refreshed_preflight" | head -n1)"
    case "$refreshed_status" in
    (2)
        jq -n --arg detail "$refreshed_preflight" '{result:"conflict", detail:$detail}'
        ;;
    (0)
        if [ "$refreshed_tree" != "$verified_tree" ]; then
            retry_json "PR #$pr base advanced after tree $verified_tree passed; refreshed tree $refreshed_tree is already certified and will be landed by the next bounded attempt. $land_detail"
        fi
        retry_json "land-pr.sh refused PR #$pr while its verified tree $verified_tree remains current and certified: $land_detail"
        ;;
    (1)
        if [ "$refreshed_tree" != "$verified_tree" ]; then
            retry_json "PR #$pr base advanced after tree $verified_tree passed; refreshed tree $refreshed_tree requires verification by the next bounded attempt. $land_detail"
        fi
        die_json "PR #$pr still resolves to verified tree $verified_tree, but that tree no longer has a qualifying release-gate receipt after landing refusal: $refreshed_preflight"
        ;;
    (*)
        die_json "merge preflight returned unexpected status $refreshed_status while reconciling PR #$pr: $refreshed_preflight"
        ;;
    esac
    exit 0
}

# Real-Git gate seam: exercise preparation, command exit and restoration
# classification without substituting GitHub or the speculative executor.
if [ "${1:-}" = --run-gate ]; then
    [ "$#" -ge 8 ] && [ "${7:-}" = -- ] \
        || die_json "private usage: verify-pr.sh --run-gate <pr> <tree> <base> <head> <worktree> -- <command...>"
    shift
    gate_args=("$1" "$2" "$3" "$4" "$5")
    shift 6
    run_verification_gate "${gate_args[@]}" "$@"
    jq -n '{result:"gate-passed"}'
    exit 0
fi

# Metadata-validation seam. The production path supplies GitHub's JSON; tests
# feed that same wire shape directly without a fake GitHub service.
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
    classify_land "$2" "$3" "$4" "$5" "" ""
fi

# Deterministic post-refusal seam. Tests supply GitHub's refreshed wire shape;
# the function fetches and reasons over real refs and production receipts.
if [ "${1:-}" = --reconcile-land-refusal ]; then
    [ "$#" -eq 8 ] \
        || die_json "private usage: verify-pr.sh --reconcile-land-refusal <refresh-status> <metadata> <pr-number> <base-name> <head-oid> <verified-tree> <detail>"
    reconcile_land_refusal "$2" "$3" "$4" "$5" "$6" "$7" "$8"
fi

# Real-Git recovery seam. The public path refreshes GitHub's base before
# entering it; tests can exercise the local proof without imitating GitHub.
if [ "${1:-}" = --recover-merged ]; then
    [ "$#" -eq 4 ] \
        || die_json "private usage: verify-pr.sh --recover-merged <base-ref> <merge-oid> <pr-number>"
    recover_merged "$2" "$3" "$4" "after verifier restart"
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
    || retry_json "could not read submitted pull request $submitted_pr from GitHub"
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
        || retry_json "could not refresh origin/$base for merged PR #$pr"
    recover_merged "$base_ref" "$merge_oid" "$pr" "after verifier restart"
fi

[ "$state" = OPEN ] || invalid_json "PR #$pr is $state, not OPEN or MERGED"
git fetch -q origin \
    "+refs/heads/$base:$base_ref" \
    "+refs/pull/$pr/head:$head_ref" \
    || retry_json "could not refresh origin/$base and PR #$pr"
head="$(git rev-parse "$head_ref" 2>/dev/null)" || die_json "could not resolve fetched PR #$pr"
[ "$head" = "$reported_head" ] || die_json "PR #$pr moved while its refs were being refreshed"

verifier_window_banner "PR #$pr — merge preflight running (computing the exact merge tree)"
gate_progress_emit_item "merge preflight" running
_preflight_start=$(date +%s)
preflight="$(activity_run "merge-preflight.sh" bash scripts/merge-preflight.sh "$base_ref" "$head_ref" 2>&1)"
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
    run_verification_gate "$pr" "$tree" "$base_ref" "$head_ref" "$verifier_wt" make test
    ;;
(*)
    gate_progress_emit_item "merge preflight" failed "seconds=$_preflight_seconds"
    die_json "merge preflight returned unexpected status $preflight_status: $preflight"
    ;;
esac

verifier_window_banner "PR #$pr — merge tree $tree passed; landing pull request"
gate_progress_emit_item "land pull request" running
_land_start=$(date +%s)
land_output="$(activity_run "land-pr.sh" bash scripts/land-pr.sh "$submitted_pr" 2>&1)"
land_status=$?
gate_progress_emit_item "land pull request" \
    "$([ "$land_status" = 0 ] && echo passed || echo failed)" \
    "seconds=$(( $(date +%s) - _land_start ))"
classify_land "$land_status" "$land_output" "$pr" "$tree" "$base" "$reported_head"
