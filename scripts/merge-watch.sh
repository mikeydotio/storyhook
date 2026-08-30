#!/usr/bin/env bash
#
# The merge gate's reconcile pass — SH-396.
#
# `scripts/merge-preflight.sh` answers "has this PR's merge tree gone green
# before" in milliseconds. This script is what makes that answer true: one
# pass over every open PR, testing whichever merge trees are still
# uncertified and reporting the result where the merge decision actually
# gets made — the PR itself.
#
# Meant to be run every 1-3 minutes (via `/loop` or a per-machine launchd
# job — installing that timer is a bootstrap step for whoever runs this, not
# something this script does itself). `make test` is 36.4s median warm on
# this machine (docs/rearch/baseline/timings.md), so running the real gate
# on every uncertified PR, every pass, is affordable; this is never a
# compile-only proxy.
#
# WHY POLLING, NOT A HOOK. A PR merged with `gh pr merge --merge` is a
# server-side merge — no push happens, so `.githooks/pre-push` never fires,
# and a merge from the GitHub web UI, another machine, or a session that
# never read CLAUDE.md is invisible to any local hook by construction. A
# poller has no such blind spot: it reaches every open PR regardless of
# where or how it will eventually be merged.
#
# WHY A PERSISTENT WORKTREE. Sharing CARGO_TARGET_DIR across worktrees is
# already ruled out (docs/spec/test-tiers.md — it breaks
# check-no-orphan-servers.sh's per-worktree scoping and non_temporary_dir's
# "beside the running test binary" rung, SH-258), so this script owns one
# dedicated worktree instead: a fresh `git worktree add` per pass would mean
# a cold `cargo build` every single poll.
#
# WHY THE COMMENT IS UPSERTED, NOT POSTED FRESH. A PR comment is what
# actually sits in the maintainer's path at the moment of the risky action
# (clicking merge) — a status file nothing reads yet, or a story filed per
# red poll, do not. But posting fresh every pass would mean a new
# notification every 1-3 minutes for a PR that is simply still broken — the
# exact self-inflicted-noise shape this project has already paid for three
# times (SH-306, SH-345, SH-263: a gate or fixture that fires repeatedly for
# one unchanged fact). So the comment is looked up by a fixed marker and
# edited in place — GitHub does not notify on an edit the way it does on a
# new comment, so a still-red PR produces exactly one notification, not one
# per poll. The comment always carries a "last checked" timestamp so the
# poller going silent (crashed, `gh` auth expired) is legible as staleness
# rather than reading as a silent all-clear — SH-306's own lesson about a
# gate that fails without saying so, one layer up.
#
# WHY EACH COMMENT ALSO CARRIES `main`'s BROWSER-TIER DISTANCE (SH-418).
# `make test` -- what this script runs -- is the merge gate, and it
# deliberately skips the browser suite (SH-394). So a CERTIFIED verdict here
# says nothing whatever about the dashboard, and until SH-418 nothing else
# said anything about it either between releases. One extra line, rendered by
# `scripts/browser-status.sh`, states how far `main` is from the last tree the
# browser suite certified -- so the reader about to click merge is told what
# this gate does NOT cover, at the moment it matters, rather than inferring
# full coverage from a green line. It is a distance, never a boolean: a dead
# browser poller and a red `main` both read as a number that grows, which is
# the SH-306 rule (silence is never a pass) applied to the tier this script
# does not run.
#
# WHY THE GITHUB SHELL HAS NO AUTOMATED TEST. The polling and comment layer is
# thin orchestration over `gh`; this project's testing tenets are explicit
# that mocking behaviour validates the mock, not the integration (SH-263,
# SH-345). The local correctness boundary is different and is tested:
# `merge-preflight.sh` decides the tree, while this script's private
# `--speculative-run` mode owns the object lease, checkout, gate-command
# environment, restoration, and cleanup. tests/merge_gate.rs drives both
# against real Git and leaves only the live GitHub API shell for hand
# verification against this repository's own PRs.

set -uo pipefail

die() {
    printf 'merge-watch: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'merge-watch: %s\n' "$1" >&2
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" \
    || die "cannot resolve the production script directory"
root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"
source_objects="$(cd "$common_dir/objects" && pwd -P)" \
    || die "cannot resolve the shared object directory"

# The real-Git core of an uncertified candidate, separated from the GitHub
# polling shell for the same reason land-pr.sh exposes --certified-run: tests
# can provoke object ownership, checkout, command failure, and signals without
# pretending a gh shim proves anything about GitHub. This is a private
# self-entry point, not an additional user-facing command.
if [ "${1:-}" = "--speculative-run" ]; then
    [ "$#" -ge 7 ] && [ "${6:-}" = "--" ] \
        || die "private usage: merge-watch.sh --speculative-run <expected-tree> <base-ref> <head-ref> <poller-worktree> -- <command...>"
    expected_tree="$2"
    base="$3"
    head="$4"
    poller_wt="$5"
    shift 6

    [ -e "$poller_wt/.git" ] \
        || die "the speculative poller worktree at $poller_wt has no .git entry"

    lease_root="$common_dir/storyhook"
    mkdir -p "$lease_root" || die "could not create private merge state at $lease_root"
    lease_root="$(cd "$lease_root" && pwd -P)" \
        || die "cannot resolve the private merge-state directory"
    lease_prefix="$lease_root/merge-watch-objects."
    lease=""
    child=""
    poller_needs_restore=0
    retain_lease=0
    inherited_alternates="${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}"

    lease_is_valid() {
        candidate="$1"
        case "$candidate" in
        "$lease_prefix"*) ;;
        *) return 1 ;;
        esac
        suffix="${candidate#"$lease_prefix"}"
        [ -n "$suffix" ] || return 1
        case "$suffix" in
        */*) return 1 ;;
        esac
        [ -d "$candidate" ] && [ ! -L "$candidate" ] || return 1
        physical="$(cd "$candidate" && pwd -P)" || return 1
        [ "$physical" = "$candidate" ]
    }

    git_private() {
        GIT_OBJECT_DIRECTORY="$lease" \
            GIT_ALTERNATE_OBJECT_DIRECTORIES="$source_objects" \
            git "$@"
    }

    restore_poller() {
        [ "$poller_needs_restore" -eq 1 ] || return 0
        if git_private -C "$poller_wt" checkout -q --detach "$base"; then
            poller_needs_restore=0
            return 0
        fi
        retain_lease=1
        return 1
    }

    cleanup() {
        if ! restore_poller; then
            note "could not restore $poller_wt to $base; retaining private objects at $lease for recovery"
            return
        fi
        if [ -n "$lease" ]; then
            if [ "$retain_lease" -eq 0 ] && lease_is_valid "$lease"; then
                if rm -rf "$lease"; then
                    lease=""
                else
                    retain_lease=1
                    note "could not remove private-object lease at $lease; retaining it for recovery"
                    return 1
                fi
            elif [ "$retain_lease" -eq 0 ]; then
                note "refusing to remove an invalid private-object lease path: $lease"
                retain_lease=1
                return 1
            fi
        fi
        return 0
    }

    on_signal() {
        signal="$1"
        if [ -n "$child" ]; then
            kill -s "$signal" "$child" 2>/dev/null || true
            wait "$child" 2>/dev/null || true
            child=""
        fi
        trap - "$signal" EXIT
        cleanup || true
        kill -s "$signal" $$
    }

    trap cleanup EXIT
    trap 'on_signal HUP' HUP
    trap 'on_signal INT' INT
    trap 'on_signal TERM' TERM

    created_lease="$(mktemp -d "$lease_prefix"XXXXXX)" \
        || die "could not create private merge object storage"
    lease="$created_lease"
    canonical_lease="$(cd "$created_lease" && pwd -P)" \
        || die "could not resolve private merge object storage"
    lease="$canonical_lease"
    lease_is_valid "$lease" \
        || die "created object storage outside the expected private location"

    candidate_tree="$(bash "$script_dir/merge-preflight.sh" \
        --object-dir "$lease" "$base" "$head")"
    preflight_status=$?
    case "$preflight_status" in
    0 | 1) ;;
    2) exit 2 ;;
    *) die "merge preflight failed with status $preflight_status" ;;
    esac
    [ "$candidate_tree" = "$expected_tree" ] \
        || die "predicted tree changed from $expected_tree to ${candidate_tree:-<none>} before the speculative run"

    merge_commit="$(git_private -C "$poller_wt" commit-tree "$candidate_tree" \
        -p "$base" -p "$head" -m "merge-watch: speculative merge")" \
        || die "could not build the speculative merge commit"
    git_private -C "$poller_wt" checkout -q --detach "$merge_commit" \
        || die "could not check out the speculative merge"
    poller_needs_restore=1

    candidate_alternates="$lease"
    if [ -n "$inherited_alternates" ]; then
        candidate_alternates="$candidate_alternates:$inherited_alternates"
    fi

    # `wait` is explicit so signal traps run immediately instead of being
    # deferred until a long gate command exits. fd 3 preserves stdin for a
    # command that needs it, matching machine-lock.sh's transparent wrapper.
    exec 3<&0
    (
        trap - HUP INT TERM
        cd "$poller_wt" || exit 1
        exec env -u GIT_OBJECT_DIRECTORY \
            GIT_ALTERNATE_OBJECT_DIRECTORIES="$candidate_alternates" \
            "$@"
    ) <&3 &
    child=$!
    exec 3<&-
    wait "$child"
    command_status=$?
    child=""

    if ! restore_poller; then
        die "the gate command finished, but the poller worktree could not be restored; private objects were retained at $lease"
    fi
    cleanup \
        || die "the gate command finished, but its private-object lease could not be removed"
    trap - EXIT HUP INT TERM
    exit "$command_status"
fi

command -v gh >/dev/null 2>&1 || die "the gh CLI is required and was not found"

repo_slug="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null)" \
    || die "could not resolve the GitHub repo slug — is gh authenticated?"

marker='<!-- storyhook-merge-gate -->'
now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

note "fetching origin/main and every open PR's head"
git fetch -q origin main \
    || die "could not fetch origin/main"
# One wildcard refspec pulls every open PR's head in a single fetch, keyed by
# number rather than branch name -- correct for a same-repo branch AND a
# fork's PR, and immune to a source branch being deleted out from under us
# mid-pass.
git fetch -q origin '+refs/pull/*/head:refs/remotes/origin/pr/*' \
    || die "could not fetch open PR heads"

# A persistent worktree so `target/` stays warm across passes -- a scratch
# worktree per pass would mean a cold compile every 1-3 minutes.
poller_wt="$common_dir/storyhook/merge-watch-worktree"
# A LINKED worktree's `.git` is a gitlink FILE pointing back at the shared
# admin dir, never a directory -- `-d` here always missed it and re-ran
# `git worktree add` on every pass, which then failed because the directory
# already existed (SH-396, found running this against this repo's own live
# PR: `-e` is the check that works for both a fresh main checkout and a
# linked worktree).
if [ ! -e "$poller_wt/.git" ]; then
    note "creating the persistent poller worktree at $poller_wt"
    git worktree add -q --detach "$poller_wt" origin/main \
        || die "could not create the poller worktree"
fi

# Looks up the id of the marker-tagged comment on a PR, if one exists yet.
# Empty output (not an error) means none exists -- the caller creates one.
find_comment_id() {
    local pr="$1"
    gh api "repos/$repo_slug/issues/$pr/comments" --paginate \
        --jq ".[] | select(.body | contains(\"storyhook-merge-gate\")) | .id" \
        2>/dev/null | tail -n1
}

# Edits the marker comment in place if one exists, else creates it. Read from
# stdin so a multi-line body never has to survive shell word-splitting -- and
# it must be `-F` (typed field), not `-f` (raw field): only `-F` documents
# `@-`/`@path` for reading a value from stdin/a file. `-f body=@-` runs
# without error and posts the four literal characters `@-` as the comment
# body (found running this against this repo's own live PR, SH-396).
upsert_comment() {
    local pr="$1" body="$2" id
    id="$(find_comment_id "$pr")"
    if [ -n "$id" ]; then
        printf '%s' "$body" \
            | gh api --method PATCH "repos/$repo_slug/issues/comments/$id" -F body=@- \
                >/dev/null \
            || note "PR #$pr: could not edit the existing status comment"
    else
        printf '%s' "$body" \
            | gh api --method POST "repos/$repo_slug/issues/$pr/comments" -F body=@- \
                >/dev/null \
            || note "PR #$pr: could not post the status comment"
    fi
}

# One sentence naming how far `main` is from the last browser-certified tree.
# Rendered once per pass, not per PR: it is a fact about `main`, identical in
# every comment, and `browser-status.sh` walks history to answer it.
browser_line() {
    local out state behind certified certified_at
    out="$(bash scripts/browser-status.sh origin/main 2>/dev/null)"
    state="$(printf '%s\n' "$out" | sed -n 's/^state=//p')"
    behind="$(printf '%s\n' "$out" | sed -n 's/^behind=//p')"
    certified="$(printf '%s\n' "$out" | sed -n 's/^certified=//p')"
    certified_at="$(printf '%s\n' "$out" | sed -n 's/^certified_at=//p')"
    case "$state" in
    current)
        printf 'green on this exact tip (certified %s)' "${certified_at:-unknown}"
        ;;
    behind)
        printf 'last green %s first-parent commit(s) back, at `%s` (certified %s)' \
            "${behind:-?}" "${certified:0:9}" "${certified_at:-unknown}"
        ;;
    never)
        printf 'NEVER — no tree in this history has passed the browser suite'
        ;;
    *)
        printf 'unknown — `scripts/browser-status.sh` could not answer'
        ;;
    esac
}

browser_status_line="$(browser_line)"
note "browser tier on main: $browser_status_line"

prs="$(gh pr list --state open --json number --jq '.[].number' 2>/dev/null)"
if [ -z "$prs" ]; then
    note "no open PRs — nothing to reconcile"
    exit 0
fi

for pr in $prs; do
    head_sha="$(git rev-parse --quiet --verify "refs/remotes/origin/pr/$pr" 2>/dev/null)"
    if [ -z "$head_sha" ]; then
        note "PR #$pr: could not resolve a fetched head — skipping this pass"
        continue
    fi

    out="$(bash scripts/merge-preflight.sh origin/main "$head_sha" 2>&1)"
    status=$?
    tree="$(printf '%s\n' "$out" | head -n1)"

    case "$status" in
    2)
        note "PR #$pr: conflict, needs rebase"
        body="$marker
### Merge-gate poller

**Result:** CONFLICT — does not merge cleanly onto \`main\`
**Browser tier on \`main\`:** $browser_status_line
**Last checked:** $now UTC

_Automated check from \`scripts/merge-watch.sh\` (SH-396). Rebase this branch onto \`main\` to clear it._"
        upsert_comment "$pr" "$body"
        ;;
    0)
        note "PR #$pr: already certified (tree $tree)"
        body="$marker
### Merge-gate poller

**Result:** CERTIFIED — this merge tree has a real \`make test\` receipt
**Merge tree:** \`$tree\`
**Browser tier on \`main\`:** $browser_status_line
**Last checked:** $now UTC

_Merging now lands a tree \`.githooks/pre-push\` already recognizes. Automated check from \`scripts/merge-watch.sh\` (SH-396)._"
        upsert_comment "$pr" "$body"
        ;;
    1)
        note "PR #$pr: uncertified (tree $tree) — running the gate"
        log="$(mktemp -t storyhook-merge-watch-log.XXXXXX)"
        if bash "$script_dir/merge-watch.sh" --speculative-run "$tree" \
            origin/main "$head_sha" "$poller_wt" -- \
            make test >"$log" 2>&1; then
            note "PR #$pr: gate passed — tree $tree is now certified"
            tail_ctx=""
            body="$marker
### Merge-gate poller

**Result:** CERTIFIED — \`make test\` just passed against this merge tree
**Merge tree:** \`$tree\`
**Browser tier on \`main\`:** $browser_status_line
**Last checked:** $now UTC

_Merging now lands a tree \`.githooks/pre-push\` already recognizes. Automated check from \`scripts/merge-watch.sh\` (SH-396)._"
        else
            note "PR #$pr: gate FAILED — see $log"
            reached="$(grep -E '^leg [^:]+:' "$log" | tail -n1)"
            tail_ctx="$(tail -n 25 "$log")"
            body="$marker
### Merge-gate poller

**Result:** RED — \`make test\` failed against this merge tree
**Merge tree:** \`$tree\`
**Last leg:** ${reached:-unknown — see the tail below}
**Browser tier on \`main\`:** $browser_status_line
**Last checked:** $now UTC

<details><summary>tail of the failing run</summary>

\`\`\`
$tail_ctx
\`\`\`
</details>

_Automated check from \`scripts/merge-watch.sh\` (SH-396). This PR must not be merged until this clears._"
        fi
        upsert_comment "$pr" "$body"
        rm -f "$log"
        ;;
    *)
        note "PR #$pr: merge-preflight.sh returned an unexpected status $status"
        ;;
    esac
done
