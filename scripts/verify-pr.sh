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
# The progress journal (SH-524) and the verifier tmux mirror (SH-545) are
# siblings of this file, never of the checkout: this script runs against
# WHATEVER checkout the daemon has registered (`current_dir(&candidate.
# checkout)`), from the bundle the daemon projected out of its own binary
# (SH-654, src/daemon/verifier_bundle.rs). A missing sibling is a packaging
# defect in that bundle and is refused by name, where the source-if-present
# shape this replaced would have run the whole verification with no progress
# and no mirror and said nothing.
# shellcheck source=gate-progress.sh
. "$script_dir/gate-progress.sh" \
    || die_json "the verifier bundle at $script_dir is missing gate-progress.sh"
# shellcheck source=verify-window.sh
. "$script_dir/verify-window.sh" \
    || die_json "the verifier bundle at $script_dir is missing verify-window.sh"
command -v jq >/dev/null 2>&1 || die_json "jq is required"
common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd -P)" \
    || die_json "could not resolve the shared git directory"
verifier_wt="$common_dir/storyhook/verification-worktree"
# Every mutating entry takes gate ownership before the lifecycle supervisor.
# Metadata-only seams remain read-only. The supervisor validates reentrancy
# against its exact recorded session, rather than trusting an environment flag.
owner_wt="$verifier_wt"
case "${1:-}" in
--run-gate) owner_wt="${6:-$verifier_wt}" ;;
--validate-metadata | --refresh-submission | --reconcile-land-refusal) owner_wt="" ;;
esac
if [ -n "$owner_wt" ] && ! python3 "$script_dir/verifier-owner.py" held "$common_dir" "$owner_wt"; then
    owner_output="$(STORYHOOK_GATE_PROGRESS_ACTIVITY_PATH="release gate" \
        bash "$script_dir/machine-lock.sh" gate -- \
        python3 "$script_dir/verifier-owner.py" run-json "$common_dir" "$owner_wt" -- \
        bash "$script_dir/verify-pr.sh" "$@")"
    owner_status=$?
    [ "$owner_status" -eq 0 ] \
        || die_json "verifier lifecycle ownership or supervision failed for $owner_wt (status $owner_status); inspect the lifecycle owner record under $common_dir/storyhook/verifier-lifecycle and the preceding diagnostics"
    printf '%s\n' "$owner_output"
    exit 0
fi

ensure_verifier_worktree() {
    fallback="$1"
    git cat-file -e "$fallback^{commit}" 2>/dev/null \
        || die_json "cannot repair the verifier worktree from unavailable commit $fallback"
    lifecycle_detail="$(python3 "$script_dir/verifier-worktree.py" ensure \
        "$common_dir" "$verifier_wt" "$fallback" 2>&1)" \
        || die_json "$lifecycle_detail"
    if [ -n "$lifecycle_detail" ]; then
        printf '%s\n' "$lifecycle_detail" >&2
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

    # Test stdout/stderr may print any error text, even Cargo-shaped JSON.
    # Only the build-only adapter owns this per-attempt evidence (SH-685).
    compiler_problem=""
    compiler_diagnostics="$(python3 "$script_dir/cargo_diagnostics.py" \
        --summarize "$failure_log.compiler.jsonl" 2>&1)" || {
        compiler_problem="$compiler_diagnostics"
        compiler_diagnostics=""
    }
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
    if [ -n "$compiler_problem" ]; then
        printf 'Compiler diagnostic collection unavailable: %.500s\n' "$compiler_problem"
    fi
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
    : >"$log.compiler.jsonl" \
        || die_json "could not create compiler diagnostic artifact for $log"
    gate_result="$(mktemp "$logs/pr-$gate_pr-result.XXXXXX")" \
        || die_json "could not create gate completion record"
    verifier_window_tail "$log"
    STORYHOOK_COMPILER_DIAGNOSTICS="$log.compiler.jsonl" \
        STORYHOOK_GATE_RESULT_FILE="$gate_result" \
        activity_run "merge-watch.sh" bash "$script_dir/merge-watch.sh" --speculative-run "$gate_tree" \
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
    # A completed red is reported through the return status rather than
    # posted here, so the caller can confirm the head it judged is still the
    # PR's head before anything is written (SH-637): `$gate_status`,
    # `$gate_tree` and `$log` stay set for `emit_tests_failed`.
    [ "$gate_status" -eq 0 ]
}

# A gate that exited 0 has certified the tree only if it minted a `gate` or
# `full` receipt on the way (`make test` and `make test-full` end in
# `gate-receipt.sh postlude`; a configured `[verify] gate` need not — SH-649).
# Landing asks `merge-preflight.sh` exactly this before it merges, so the same
# reader is asked here, right after the gate, rather than a second parser of
# the receipt file; without this the refusal surfaced downstream, from
# `reconcile_land_refusal`, as "no longer has a qualifying release-gate
# receipt" — a diagnosis about the wrong layer, after GitHub had been asked
# again. Refused by name: the gate, the tree, and what a gate must do.
#
# It is asked about the PINNED parents the gate ran on, never the refs
# (SH-666): asked about `refs/remotes/origin/<base>` after a fetch had moved
# it, it recomputed a different merge, met a real conflict, and halted the
# whole queue as "certified nothing" — over a tree it had in fact certified.
require_certified_by_gate() {
    certified_tree="$1"
    certified_base="$2"
    certified_head="$3"
    recheck="$(bash "$script_dir/merge-preflight.sh" "$certified_base" "$certified_head" 2>&1)"
    recheck_status=$?
    recheck_tree="$(printf '%s\n' "$recheck" | head -n1)"
    if [ "$recheck_status" -eq 0 ] && [ "$recheck_tree" = "$certified_tree" ]; then
        return 0
    fi
    gate_progress_emit_item "release gate" failed
    die_json "gate \`$gate_display\` exited 0 on merge tree \`$certified_tree\` but certified nothing: $(printf '%s\n' "$recheck" | tail -n +2). In the configured [verify] gate script, call \"\$STORYHOOK_GATE_RECEIPT\" preflight before testing and \"\$STORYHOOK_GATE_RECEIPT\" postlude gate (or postlude full) only after all required tests pass. The verifier supplies this portable writer; no StoryHook scripts or Git hooks are needed in the project. StoryHook's own scripts/gate-receipt.sh postlude remains supported. A changed receipt or a bare successful test runner cannot certify a merge. Gate log: $log"
}

# Posts the red verdict for the gate `run_verification_gate` just reported as
# failed. The wire shape is exactly the one it used to emit itself; only the
# moment moved, to after the caller's head confirmation.
emit_tests_failed() {
    detail="$(verification_failure_detail "$gate_status" "$log")"
    jq -n --arg tree "$gate_tree" --arg log "$log" --arg detail "$detail" \
        '{result:"tests-failed", tree:$tree, log:$log, detail:$detail}'
    exit 0
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
        refreshed_metadata="$(gh pr view "$landed_pr" --json number,state,isDraft,isCrossRepository,baseRefName,headRefName,headRefOid,mergeCommit 2>/dev/null)"
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
    head_branch="$(printf '%s' "$metadata" | jq -er '.headRefName')" \
        || die_json "PR #$pr returned no head branch"
    [ "$draft" = false ] || invalid_json "PR #$pr is a draft"
    [ "$cross" = false ] \
        || invalid_json "PR #$pr comes from a fork; centralized verification accepts same-repository PRs only"
}

# Refreshes the base and PR head refs and refuses to preflight until GitHub's
# own picture of the PR head has caught up with the branch it mirrors (SH-636).
#
# `refs/pull/N/head` and the API's `headRefOid` are both projections of the
# PR's head branch, written by GitHub's asynchronous post-push pipeline, and
# they lag TOGETHER: comparing one against the other passes vacuously in the
# exact window this function exists for. `refs/heads/<branch>` is the source
# those projections mirror and is updated synchronously by the push, so it is
# the one answer the caller's push provably moved. Measured on SH-630 / PR
# #737: the verifier read both projections ~1s after the push and got the
# pre-push head from each; the pull ref caught up 16s later.
#
# Sets the globals the public path reads afterwards: `base_ref`, `head_ref`
# (the local remote-tracking names preflight merges) and `head` (the agreed
# oid). Any disagreement is reported as RETRYABLE, never as a conflict and
# never as permanent — the daemon's own bounded cadence
# (`INFRASTRUCTURE_RETRY_ATTEMPTS`) re-asks; a lag that outlasts that budget
# halts loudly with the three oids in the detail. The branch tip is read with
# `ls-remote` rather than fetched so no remote-tracking ref for the feature
# branch is written into the registered checkout.
refresh_submission_refs() {
    refresh_pr="$1"
    refresh_base="$2"
    refresh_branch="$3"
    refresh_reported_head="$4"
    base_ref="refs/remotes/origin/$refresh_base"
    head_ref="refs/remotes/origin/pr/$refresh_pr"

    git fetch -q origin \
        "+refs/heads/$refresh_base:$base_ref" \
        "+refs/pull/$refresh_pr/head:$head_ref" \
        || retry_json "could not refresh origin/$refresh_base and PR #$refresh_pr"
    head="$(git rev-parse "$head_ref" 2>/dev/null)" \
        || die_json "could not resolve fetched PR #$refresh_pr"

    # Always the fully qualified name: `--exit-code` matches by suffix, so a
    # bare branch name could be answered by a tag spelled the same way.
    branch_listing="$(git ls-remote --exit-code origin "refs/heads/$refresh_branch" 2>/dev/null)"
    case "$?" in
    (0) ;;
    (2)
        invalid_json "PR #$refresh_pr's head branch refs/heads/$refresh_branch does not exist on origin; push it"
        ;;
    (*)
        retry_json "could not read refs/heads/$refresh_branch on origin for PR #$refresh_pr"
        ;;
    esac
    branch_tip="$(printf '%s\n' "$branch_listing" | awk 'NR == 1 { print $1 }')"
    [ -n "$branch_tip" ] \
        || die_json "origin listed refs/heads/$refresh_branch for PR #$refresh_pr without an oid"

    if [ "$head" != "$refresh_reported_head" ] || [ "$head" != "$branch_tip" ]; then
        retry_json "PR #$refresh_pr's head has not converged on its branch yet: refs/pull/$refresh_pr/head fetched as $head, GitHub reports headRefOid $refresh_reported_head, and refs/heads/$refresh_branch is at $branch_tip. GitHub updates a pull request's head asynchronously after a branch push, so this reading is not the PR's conflict state and no preflight ran. The verifier retries on its own; if this halts, acknowledge the incident from the dashboard's verification banner once the three agree, or move the story to verifying again."
    fi
}

# Confirms, immediately before a verdict is posted, that the head it judged is
# still the PR's head (SH-637).
#
# `refresh_submission_refs` makes the head current at the START of an attempt;
# nothing made it current at the END, and a verdict is a statement about a
# head. Preflight is quick, but the release gate runs for minutes, and a push
# that lands inside either window turns a true CONFLICT or RED into a verdict
# about a commit nobody can act on — the same stale-report shape SH-636 fixed
# at entry, arriving through the other door. Measured on SH-622 / PR #741 and
# SH-625 / PR #740: three such verdicts in one session, each costing a full
# implementer turn to prove that nothing was wrong.
#
# Re-reads GitHub, requires the same PR and base (anything else is an
# identity change, as `reconcile_land_refusal` already rules), requires OPEN
# (a PR that merged or closed meanwhile is the next attempt's entry path to
# classify, so that is a retry), then asks `refresh_submission_refs` for the
# converged head and requires it to be the one that was judged. A moved head
# is RETRYABLE, never a verdict: the daemon's own cadence re-verifies the new
# head, which is the only head a verdict could be about.
#
#   confirm_judged_head <pr> <base-name> <judged-head> <verdict> [<extra>]
confirm_judged_head() {
    judged_pr="$1"
    judged_base="$2"
    judged_head="$3"
    judged_verdict="$4"
    judged_extra="${5:-}"
    current_metadata="$(gh pr view "$judged_pr" --json number,state,isDraft,isCrossRepository,baseRefName,headRefName,headRefOid,mergeCommit 2>/dev/null)" \
        || retry_json "could not re-read PR #$judged_pr from GitHub before posting its $judged_verdict verdict; the verdict was not posted and the next attempt re-verifies.${judged_extra:+ $judged_extra}"
    validate_metadata "$current_metadata"
    [ "$pr" = "$judged_pr" ] \
        || invalid_json "PR #$judged_pr was re-read as PR #$pr before its $judged_verdict verdict"
    [ "$base" = "$judged_base" ] \
        || invalid_json "PR #$pr changed base branch from $judged_base to $base while it was being verified"
    [ "$state" = OPEN ] \
        || retry_json "PR #$pr is $state, not OPEN, now that its $judged_verdict verdict is ready; the verdict was not posted and the next attempt classifies the $state pull request from the start.${judged_extra:+ $judged_extra}"
    refresh_submission_refs "$pr" "$base" "$head_branch" "$reported_head"
    [ "$head" = "$judged_head" ] \
        || retry_json "PR #$pr head moved from $judged_head to $head while it was being verified; the $judged_verdict verdict for the superseded head was not posted, and the next attempt verifies the new head.${judged_extra:+ $judged_extra}"
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
    run_verification_gate "${gate_args[@]}" "$@" || emit_tests_failed
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

# Head-convergence seam (SH-636). Tests supply GitHub's wire shape for the
# submitted PR; the function fetches from a real local remote and reads the
# branch tip the pull ref mirrors, without a GitHub imitation.
if [ "${1:-}" = --refresh-submission ]; then
    [ "$#" -eq 2 ] \
        || die_json "private usage: verify-pr.sh --refresh-submission <json>"
    validate_metadata "$2"
    refresh_submission_refs "$pr" "$base" "$head_branch" "$reported_head"
    jq -n --arg head "$head" '{result:"refs-current", head:$head}'
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

# The gate is an argument, never a default of this script's own (SH-649):
# the daemon reads the project's `[verify] gate` from its pointer file and
# `GateCommand::DEFAULT` is the one place `make test` lives. A caller that
# names no gate is refused by name rather than handed one it did not choose.
[ "$#" -ge 3 ] && [ "$2" = -- ] \
    || die_json "usage: verify-pr.sh <pr-url> -- <gate-command...> (the daemon passes the project's [verify] gate)"
submitted_pr="$1"
shift 2
gate_command=("$@")
gate_display="$*"
command -v gh >/dev/null 2>&1 || die_json "the gh CLI is required"

verifier_window_banner "verifying $submitted_pr — checking pull request metadata"
gate_progress_emit_item "pull request metadata" running
_pr_meta_start=$(date +%s)
metadata="$(gh pr view "$submitted_pr" --json number,state,isDraft,isCrossRepository,baseRefName,headRefName,headRefOid,mergeCommit 2>/dev/null)" \
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
gate_progress_emit_item "pull request refs" running
_refs_start=$(date +%s)
# Every exit inside the refresh is a JSON verdict at exit 0, so a row left
# `running` here means exactly that the refs did not agree or could not be
# read — the retry comment carries the detail.
refresh_submission_refs "$pr" "$base" "$head_branch" "$reported_head"
gate_progress_emit_item "pull request refs" passed "seconds=$(( $(date +%s) - _refs_start ))"

# The transaction is pinned to two COMMITS from here on, never re-read from
# the refs (SH-584's rule, and SH-666's second incident): `refs/remotes/
# origin/<base>` is shared with every other process in this repository — a
# `/story do` creating a worktree, a poller, another verification — and any
# fetch moves it while the gate runs. The preflight, the gate and the
# certification check below must all speak about the same two parents, or
# a base that merely moved reads as a gate that certified nothing. A base
# that has moved by landing time is landing's to find, under the merge lock,
# where it is answered as the story's own CONFLICT (or a fresh tree to
# verify), never as a halt of the verifier.
base_commit="$(git rev-parse --verify "$base_ref^{commit}" 2>/dev/null)" \
    || die_json "could not resolve $base_ref to a commit after refreshing PR #$pr"
head_commit="$(git rev-parse --verify "$head_ref^{commit}" 2>/dev/null)" \
    || die_json "could not resolve $head_ref to a commit after refreshing PR #$pr"

verifier_window_banner "PR #$pr — merge preflight running (computing the exact merge tree)"
gate_progress_emit_item "merge preflight" running
_preflight_start=$(date +%s)
preflight="$(activity_run "merge-preflight.sh" bash "$script_dir/merge-preflight.sh" "$base_commit" "$head_commit" 2>&1)"
preflight_status=$?
tree="$(printf '%s\n' "$preflight" | head -n1)"
_preflight_seconds=$(( $(date +%s) - _preflight_start ))
case "$preflight_status" in
(2)
    gate_progress_emit_item "merge preflight" failed "seconds=$_preflight_seconds"
    confirm_judged_head "$pr" "$base" "$head" conflict
    jq -n --arg detail "$preflight" '{result:"conflict", detail:$detail}'
    exit 0
    ;;
(0)
    gate_progress_emit_item "merge preflight" passed "seconds=$_preflight_seconds"
    gate_progress_emit_item "release gate" reused
    verifier_window_banner "PR #$pr — merge tree $tree already certified; release gate reused, no live \`$gate_display\` output for this run"
    ;;
(1)
    gate_progress_emit_item "merge preflight" passed "seconds=$_preflight_seconds"
    run_verification_gate "$pr" "$tree" "$base_commit" "$head_commit" "$verifier_wt" "${gate_command[@]}" || {
        confirm_judged_head "$pr" "$base" "$head" red "Gate log of the superseded attempt: $log"
        emit_tests_failed
    }
    require_certified_by_gate "$tree" "$base_commit" "$head_commit"
    ;;
(*)
    gate_progress_emit_item "merge preflight" failed "seconds=$_preflight_seconds"
    die_json "merge preflight returned unexpected status $preflight_status: $preflight"
    ;;
esac

verifier_window_banner "PR #$pr — merge tree $tree passed; landing pull request"
gate_progress_emit_item "land pull request" running
_land_start=$(date +%s)
land_output="$(activity_run "land-pr.sh" bash "$script_dir/land-pr.sh" "$submitted_pr" 2>&1)"
land_status=$?
gate_progress_emit_item "land pull request" \
    "$([ "$land_status" = 0 ] && echo passed || echo failed)" \
    "seconds=$(( $(date +%s) - _land_start ))"
classify_land "$land_status" "$land_output" "$pr" "$tree" "$base" "$reported_head"
