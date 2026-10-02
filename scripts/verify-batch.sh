#!/usr/bin/env bash
#
# Publish or retire one verification batch's branch and pull request (SH-831).
#
#   verify-batch.sh publish <branch> <tip> <base> <title> <body>
#   verify-batch.sh retire <branch> <pr-url|-> <comment>
#   verify-batch.sh base-policy <base>
#   verify-batch.sh prune-members (<pr-url> <branch> <head>)...
#
# The daemon assembles a batch's merge commits itself (src/service/
# batch_assembly.rs); this script is the batch's only contact with GitHub, so
# every transport and pull-request call goes through the origin-pinned
# adapter (github_git, github_exec) and its gh credential helper, exactly as
# the story helper's submit verb and verify-pr.sh do. It runs from the head
# story's lease repository, where the merge commits were written, with
# STORYHOOK_GITHUB_AUTHORITY naming the registered checkout.
#
# publish pushes <tip> to refs/heads/<branch> unless origin already has it
# there (never with force; an existing branch at another commit is refused),
# then adopts the one open same-repository pull request for the branch or
# opens it against <base>. It answers only once GitHub reports the pull
# request's head as <tip>: verify-pr.sh reads a head that has not converged as
# a retryable failure, and a batch gate that starts too early would waste the
# batch. retire closes the pull request if it is open (reporting it when a
# person already merged it) and deletes the branch on origin if it is there;
# running it again changes nothing.
#
# base-policy answers whether <base> requires signed commits (SH-832 D8). A
# batch's merge commits are unsigned (a signer must never prompt inside the
# daemon), so GitHub would refuse to land it after the attempt marker is
# written, which leaves every member fenced as uncertain. Rulesets are read
# with read access; classic protection's signature rule only with admin
# rights, and GitHub answers 404 to anyone else as it does for an
# unprotected branch, so a 404 there reads as no classic requirement. Any
# other failure is a refusal: the daemon then forms no batch.
#
# prune-members deletes, after a batch landed, each member's own branch on
# origin: only when GitHub reports that member's pull request MERGED from
# that same-repository branch at <head>, and origin still has the branch at
# <head>. Anything else keeps the branch and says why (unmerged, moved,
# absent, other-head, unreadable); running it again changes nothing. Like
# land-pr.sh's own branch delete, the read and the delete are two steps: the
# member's agent session is reaped before this runs.
#
# Exactly one JSON document on stdout: {"ok":true,...} with exit 0, or
# {"ok":false,"reason":"<slug>","display":"<sentence>"} with exit 1.

set -uo pipefail

# How long publish waits for GitHub to report the new head, and how often it
# asks. STORYHOOK_BATCH_CONVERGENCE_SECS lets a fixture take the refusal
# without waiting out the production bound.
BATCH_CONVERGENCE_SECS="${STORYHOOK_BATCH_CONVERGENCE_SECS:-60}"
BATCH_CONVERGENCE_POLL_SECS=2

refuse() {
    jq -n --arg reason "$1" --arg display "$2" '{ok:false, reason:$reason, display:$display}'
    exit 1
}

command -v jq >/dev/null 2>&1 || {
    printf '%s\n' '{"ok":false,"reason":"jq-missing","display":"verify-batch.sh: jq is required"}'
    exit 1
}
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" \
    || refuse bundle-missing "verify-batch.sh: cannot resolve its own directory"
# shellcheck source=python-runtime.sh
. "$script_dir/python-runtime.sh" || refuse bundle-missing "missing Python runtime policy in $script_dir"
storyhook_python_init || refuse python-runtime "$STORYHOOK_PYTHON_ERROR"
# shellcheck source=github-access.sh
. "$script_dir/github-access.sh" \
    || refuse bundle-missing "verify-batch.sh: the verifier bundle at $script_dir is missing github-access.sh"
# shellcheck source=gate-progress.sh
. "$script_dir/gate-progress.sh" \
    || refuse bundle-missing "verify-batch.sh: the verifier bundle at $script_dir is missing gate-progress.sh"

case "$BATCH_CONVERGENCE_SECS" in
'' | *[!0-9]*) refuse usage "verify-batch.sh: STORYHOOK_BATCH_CONVERGENCE_SECS must be whole seconds, not \`$BATCH_CONVERGENCE_SECS\`" ;;
esac

valid_branch() {
    printf '%s' "$1" | grep -Eqx 'storyhook/verify-batch/[0-9a-f]{12}'
}

valid_oid() {
    printf '%s' "$1" | grep -Eqx '[0-9a-f]{40}|[0-9a-f]{64}'
}

# remote_head <branch> — the exact commit origin has for the branch, or empty
# when it has none. A failed read is a failure, never an absent branch.
remote_head() {
    local ref="refs/heads/$1" out
    out="$(github_git ls-remote --heads origin "$ref" 2>&1)" || {
        printf '%s\n' "$out" >&2
        return 1
    }
    printf '%s\n' "$out" | awk -F '\t' -v ref="$ref" '$2 == ref { print $1 }'
}

publish() {
    [ "$#" -eq 5 ] || refuse usage "usage: verify-batch.sh publish <branch> <tip> <base> <title> <body>"
    local branch="$1" tip="$2" base="$3" title="$4" body="$5"
    valid_branch "$branch" || refuse invalid-branch "verify-batch.sh: \`$branch\` is not a batch branch"
    valid_oid "$tip" || refuse invalid-tip "verify-batch.sh: \`$tip\` is not a full commit id"
    [ -n "$base" ] || refuse invalid-base "verify-batch.sh: no base branch was named"
    github_begin || refuse origin-unavailable "verify-batch.sh: cannot establish the GitHub origin: ${GITHUB_ACCESS_ERROR:-origin unavailable}"
    gate_progress_emit_item "batch publication" running

    local before after push_out pushed=false
    before="$(remote_head "$branch" 2>&1)" \
        || refuse remote-read-failed "verify-batch.sh: reading origin/$branch before the push failed: $before"
    if [ "$before" != "$tip" ]; then
        [ -z "$before" ] \
            || refuse branch-exists "verify-batch.sh: origin already has $branch at $before, not the batch tip $tip"
        push_out="$(github_git push origin "$tip:refs/heads/$branch" 2>&1)" \
            || refuse push-failed "verify-batch.sh: pushing $branch to origin failed: $push_out"
        pushed=true
    fi
    after="$(remote_head "$branch" 2>&1)" \
        || refuse remote-read-failed "verify-batch.sh: reading origin/$branch after the push failed: $after"
    [ "$after" = "$tip" ] \
        || refuse push-unverified "verify-batch.sh: origin/$branch is at \`${after:-<absent>}\` after the push, not the batch tip $tip"

    local fields=number,url,baseRefName,headRefOid,isCrossRepository
    local listed open wrong_base count url adopted
    listed="$(github_exec pr list --head "$branch" --state open --json "$fields" --limit 20 2>&1)" \
        || refuse pull-request-unlisted "verify-batch.sh: gh could not list pull requests for $branch: $listed"
    open="$(printf '%s' "$listed" | jq -c '[.[] | select(.isCrossRepository == false)]' 2>/dev/null)" \
        || refuse pull-request-unlisted "verify-batch.sh: gh pr list returned something other than JSON: $listed"
    wrong_base="$(printf '%s' "$open" | jq -r --arg b "$base" '[.[] | select(.baseRefName != $b) | "#\(.number) targets \(.baseRefName)"] | join("; ")')"
    [ -z "$wrong_base" ] \
        || refuse wrong-base-pull-request "verify-batch.sh: an open pull request for $branch does not target $base: $wrong_base"
    count="$(printf '%s' "$open" | jq 'length')"
    case "$count" in
    0)
        url="$(github_exec pr create --base "$base" --head "$branch" --title "$title" --body "$body" 2>&1)" \
            || refuse pull-request-uncreated "verify-batch.sh: gh pr create failed for $branch: $url"
        url="$(printf '%s\n' "$url" | grep -E '^https?://' | tail -n 1)"
        [ -n "$url" ] || refuse pull-request-uncreated "verify-batch.sh: gh pr create printed no URL for $branch"
        adopted=false
        ;;
    1)
        url="$(printf '%s' "$open" | jq -r '.[0].url')"
        adopted=true
        ;;
    *)
        refuse multiple-pull-requests "verify-batch.sh: more than one open pull request comes from $branch: $(printf '%s' "$open" | jq -r 'map("#\(.number)") | join(", ")')"
        ;;
    esac

    local deadline view head number state
    deadline=$(($(date +%s) + BATCH_CONVERGENCE_SECS))
    while :; do
        view="$(github_exec pr view "$url" --json number,url,state,baseRefName,headRefOid 2>&1)" \
            || refuse pull-request-unreadable "verify-batch.sh: gh could not read back $url: $view"
        head="$(printf '%s' "$view" | jq -r '.headRefOid // empty' 2>/dev/null)"
        number="$(printf '%s' "$view" | jq -r '.number // empty' 2>/dev/null)"
        state="$(printf '%s' "$view" | jq -r '.state // empty' 2>/dev/null)"
        [ "$state" = OPEN ] || refuse pull-request-not-open "verify-batch.sh: pull request $url is ${state:-unreadable}, not OPEN"
        [ "$head" = "$tip" ] && break
        [ "$(date +%s)" -lt "$deadline" ] \
            || refuse head-unconverged "verify-batch.sh: after ${BATCH_CONVERGENCE_SECS}s GitHub still reports $url at ${head:-<none>}, not the batch tip $tip"
        sleep "$BATCH_CONVERGENCE_POLL_SECS"
    done
    gate_progress_emit_item "batch publication" passed
    jq -n --arg url "$url" --argjson number "$number" --arg base "$base" --arg head "$tip" \
        --argjson adopted "$adopted" --argjson pushed "$pushed" \
        '{ok:true, url:$url, number:$number, base:$base, head_oid:$head, adopted:$adopted, pushed:$pushed}'
}

retire() {
    [ "$#" -eq 3 ] || refuse usage "usage: verify-batch.sh retire <branch> <pr-url|-> <comment>"
    local branch="$1" url="$2" comment="$3"
    valid_branch "$branch" || refuse invalid-branch "verify-batch.sh: \`$branch\` is not a batch branch"
    github_begin || refuse origin-unavailable "verify-batch.sh: cannot establish the GitHub origin: ${GITHUB_ACCESS_ERROR:-origin unavailable}"

    local closed=false merged=false view state out
    if [ "$url" != - ]; then
        view="$(github_exec pr view "$url" --json state 2>&1)" \
            || refuse pull-request-unreadable "verify-batch.sh: gh could not read $url: $view"
        state="$(printf '%s' "$view" | jq -r '.state // empty' 2>/dev/null)"
        case "$state" in
        OPEN)
            out="$(github_exec pr close "$url" --comment "$comment" 2>&1)" \
                || refuse pull-request-unclosed "verify-batch.sh: gh could not close $url: $out"
            closed=true
            ;;
        MERGED) merged=true ;;
        CLOSED) ;;
        *) refuse pull-request-unreadable "verify-batch.sh: $url reported state \`$state\`" ;;
        esac
    fi

    local present deleted=false
    present="$(remote_head "$branch" 2>&1)" \
        || refuse remote-read-failed "verify-batch.sh: reading origin/$branch failed: $present"
    if [ -n "$present" ]; then
        out="$(github_git push origin ":refs/heads/$branch" 2>&1)" \
            || refuse branch-undeleted "verify-batch.sh: deleting $branch on origin failed: $out"
        deleted=true
    fi
    jq -n --argjson closed "$closed" --argjson merged "$merged" --argjson deleted "$deleted" \
        '{ok:true, closed:$closed, merged:$merged, deleted:$deleted}'
}

base_policy() {
    [ "$#" -eq 1 ] || refuse usage "usage: verify-batch.sh base-policy <base>"
    local base="$1" repo rules signed out
    [ -n "$base" ] || refuse invalid-base "verify-batch.sh: no base branch was named"
    github_begin || refuse origin-unavailable "verify-batch.sh: cannot establish the GitHub origin: ${GITHUB_ACCESS_ERROR:-origin unavailable}"
    repo="${STORYHOOK_GITHUB_EXPECTED#*/}"
    rules="$(github_exec api "repos/$repo/rules/branches/$base" 2>&1)" \
        || refuse base-policy-unreadable "verify-batch.sh: gh could not read the rulesets of $base: $rules"
    signed="$(printf '%s' "$rules" | jq -r 'if type == "array" then any(.[]; .type == "required_signatures") else error("not a list") end' 2>/dev/null)" \
        || refuse base-policy-unreadable "verify-batch.sh: the rulesets of $base are not a JSON list: $rules"
    if [ "$signed" != true ]; then
        if out="$(github_exec api "repos/$repo/branches/$base/protection/required_signatures" 2>&1)"; then
            signed="$(printf '%s' "$out" | jq -r '.enabled == true' 2>/dev/null)" \
                || refuse base-policy-unreadable "verify-batch.sh: the signature protection of $base is not JSON: $out"
        elif printf '%s' "$out" | grep -q 'HTTP 404'; then
            signed=false
        else
            refuse base-policy-unreadable "verify-batch.sh: gh could not read the signature protection of $base: $out"
        fi
    fi
    jq -n --argjson signed "$signed" '{ok:true, signatures_required:$signed}'
}

prune_members() {
    { [ "$#" -gt 0 ] && [ $(($# % 3)) -eq 0 ]; } \
        || refuse usage "usage: verify-batch.sh prune-members (<pr-url> <branch> <head>)..."
    github_begin || refuse origin-unavailable "verify-batch.sh: cannot establish the GitHub origin: ${GITHUB_ACCESS_ERROR:-origin unavailable}"
    local results="[]" url branch head view result detail present out
    while [ "$#" -gt 0 ]; do
        url="$1" branch="$2" head="$3"
        shift 3
        valid_oid "$head" || refuse invalid-head "verify-batch.sh: \`$head\` is not a full commit id"
        detail=""
        if ! view="$(github_exec pr view "$url" --json state,headRefName,headRefOid,isCrossRepository 2>&1)"; then
            result=unreadable detail="$view"
        elif [ "$(printf '%s' "$view" | jq -r '.state // empty' 2>/dev/null)" != MERGED ]; then
            result=unmerged
        elif [ "$(printf '%s' "$view" | jq -r '"\(.headRefName)|\(.headRefOid)|\(.isCrossRepository)"' 2>/dev/null)" != "$branch|$head|false" ]; then
            result=other-head detail="$view"
        elif ! present="$(remote_head "$branch" 2>&1)"; then
            result=unreadable detail="$present"
        elif [ -z "$present" ]; then
            result=absent
        elif [ "$present" != "$head" ]; then
            result=moved detail="origin has $present"
        elif out="$(github_git push origin ":refs/heads/$branch" 2>&1)"; then
            result=deleted
        else
            result=unreadable detail="$out"
        fi
        results="$(printf '%s' "$results" | jq -c --arg b "$branch" --arg r "$result" --arg d "$detail" '. + [{branch:$b, result:$r} + (if $d == "" then {} else {detail:$d} end)]')"
    done
    jq -n --argjson members "$results" '{ok:true, members:$members}'
}

case "${1:-}" in
publish) shift; publish "$@" ;;
retire) shift; retire "$@" ;;
base-policy) shift; base_policy "$@" ;;
prune-members) shift; prune_members "$@" ;;
*) refuse usage "usage: verify-batch.sh publish|retire|base-policy|prune-members ..." ;;
esac
