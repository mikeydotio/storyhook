#!/usr/bin/env bash
#
# The producer half of the coverage tier's detection layer — SH-429.
#
# One pass: if `origin/main`'s tip tree has no coverage map yet
# (`scripts/coverage-status.sh`'s own decision — this script asks it and
# obeys, never re-deciding), check that tip out in a persistent, locked
# worktree, ensure it carries a `gate`/`full` receipt (running `make test`
# there if it does not — the ordinary case is that it already does, since
# `scripts/merge-watch.sh` already certified it on the way to landing, per
# SH-396's own merge gate; a fresh machine or a merge from elsewhere is the
# case this step exists for), and run `scripts/coverage-map.sh` against it.
#
# This is the SAME shape as `scripts/browser-watch.sh` on purpose — a council
# verdict on SH-429 (recorded as a story comment) chose it explicitly over
# piggybacking coverage capture onto every local `make test`'s own postlude,
# for the identical reason SH-418's council already gave for the browser
# tier: a per-worktree hook fires far more often than needed while doing
# nothing to guarantee freshness relative to `main`'s actual tip, which is
# the fact that matters for what a MERGE (and therefore `select-tests.sh`'s
# own baseline resolution) will see.
#
# ITS OWN WORKTREE AND ITS OWN LOCK, separate from `browser-watch-worktree`:
# an instrumented build lives in a SEPARATE `target-coverage/` (`coverage-
# map.sh`'s own header explains why -- `-C instrument-coverage` changes every
# fingerprint), so sharing the browser tier's worktree would mean the two
# pollers evict each other's warm build on every alternating run.
#
# USAGE
#   coverage-watch.sh [--plan] [--ref REF]
#
#   --plan      report the decision and the exact command a real pass would
#               run, then stop.
#   --ref REF   ask about REF instead of `origin/main`. Suppresses the fetch.
#
# EXIT CODES
#   0   nothing to do (already current, or another pass holds the lock), or
#       the capture ran and succeeded
#   1   the gate run or the coverage capture failed against this tree
#   2   refused to run (a git failure)

set -uo pipefail

die() {
    printf 'coverage-watch: %s\n' "$1" >&2
    exit 2
}

note() {
    printf 'coverage-watch: %s\n' "$1" >&2
}

plan=0
ref=""
while [ "$#" -gt 0 ]; do
    case "$1" in
    --plan) plan=1; shift ;;
    --ref)
        shift
        [ "$#" -gt 0 ] || die "--ref needs a ref"
        ref="$1"; shift
        ;;
    *) die "unknown argument '$1' -- usage: coverage-watch.sh [--plan] [--ref REF]" ;;
    esac
done

script_dir="$(cd "$(dirname "$0")" && pwd)" || die "cannot resolve this script's directory"

root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"

if [ -z "$ref" ]; then
    ref="origin/main"
    note "fetching $ref"
    git fetch -q origin main || die "could not fetch origin/main"
fi

status_out="$(bash "$script_dir/coverage-status.sh" "$ref" 2>/dev/null)"
status_code=$?
[ "$status_code" -le 2 ] || die "coverage-status.sh could not answer for '$ref'"

tip="$(printf '%s\n' "$status_out" | sed -n 's/^tip=//p')"
tip_tree="$(printf '%s\n' "$status_out" | sed -n 's/^tip_tree=//p')"
state="$(printf '%s\n' "$status_out" | sed -n 's/^state=//p')"

run_cmd=(bash scripts/coverage-map.sh)

if [ "$state" = "current" ]; then
    note "up to date — $ref's tree ($tip_tree) already has a coverage map"
    [ "$plan" = "1" ] && printf 'decision=none\n'
    exit 0
fi

if [ "$plan" = "1" ]; then
    printf 'decision=run\n'
    printf 'ref=%s\n' "$ref"
    printf 'tip=%s\n' "$tip"
    printf 'tip_tree=%s\n' "$tip_tree"
    printf 'state=%s\n' "$state"
    printf 'command=%s\n' "${run_cmd[*]}"
    note "would run '${run_cmd[*]}' against $ref (${tip:0:9}) — state=$state"
    exit 0
fi

lock="$common_dir/storyhook/coverage-watch.lock"
mkdir -p "$common_dir/storyhook" || die "could not create $common_dir/storyhook"
if ! mkdir "$lock" 2>/dev/null; then
    holder="$(cat "$lock/pid" 2>/dev/null || true)"
    if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
        note "a pass is already running (pid $holder) — leaving it to finish"
        exit 0
    fi
    note "clearing a lock left by pid ${holder:-unknown}, which is no longer running"
    rm -rf "$lock" || die "could not clear the stale lock at $lock"
    mkdir "$lock" 2>/dev/null || die "could not take the lock at $lock"
fi
printf '%s\n' "$$" >"$lock/pid"
trap 'rm -rf "$lock"' EXIT

worktree="$common_dir/storyhook/coverage-watch-worktree"
if [ ! -e "$worktree/.git" ]; then
    note "creating the persistent coverage-tier worktree at $worktree"
    git worktree add -q --detach "$worktree" "$tip" \
        || die "could not create the coverage-tier worktree"
fi
git -C "$worktree" checkout -q --detach "$tip" \
    || die "could not check $tip out in $worktree"

reports="$common_dir/storyhook/coverage-watch-reports"
mkdir -p "$reports" || die "could not create $reports"
day="$(date -u +%Y-%m-%d)"
started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

receipts="$common_dir/storyhook/gate-receipts"
existing_tier=""
if [ -f "$receipts/$tip_tree" ]; then
    existing_tier="$(sed -n 's/^tier //p' "$receipts/$tip_tree" 2>/dev/null | head -n1)"
    existing_tier="${existing_tier:-gate}"
fi

if [ "$existing_tier" != "gate" ] && [ "$existing_tier" != "full" ]; then
    note "$tip_tree carries no gate/full receipt yet — running 'make test' in \
$worktree first (the ordinary case: main's merges are already certified by \
scripts/merge-watch.sh; this only fires on a fresh machine or a merge landed \
from elsewhere)"
    if ! (cd "$worktree" && make test); then
        finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '%s %s tip=%s tree=%s stage=gate exit=1\n' \
            "$started" "$finished" "$tip" "$tip_tree" >>"$reports/$day.log"
        note "RED — 'make test' failed against ${tip:0:9}; no coverage map was \
captured. Logged to $reports/$day.log"
        exit 1
    fi
fi

note "running '${run_cmd[*]}' against ${tip:0:9} (tree $tip_tree) in $worktree"
(cd "$worktree" && "${run_cmd[@]}")
run_status=$?
finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

printf '%s %s tip=%s tree=%s stage=coverage-map exit=%s\n' \
    "$started" "$finished" "$tip" "$tip_tree" "$run_status" >>"$reports/$day.log"

if [ "$run_status" -eq 0 ]; then
    note "green — tree $tip_tree now has a coverage map"
    exit 0
fi

note "RED — '${run_cmd[*]}' failed against ${tip:0:9}. Logged to $reports/$day.log"
exit 1
