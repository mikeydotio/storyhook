#!/usr/bin/env bash
#
# The producer half of the browser tier's detection layer — SH-418.
#
# SH-394 split the gates: `make test` protects `main`, `make test-full` adds
# the browser suite and protects a release. Nothing then ran the release tier
# between releases, so a dashboard regression could merge and sit red until
# somebody tried to cut a release, at which point it blocked one. SH-416 is
# the measured case (a restructured drawer banner left
# `blocked-drop-reason.spec.ts` asserting the shape it replaced; found by an
# unrelated story running the suite incidentally, not by any gate).
#
# One pass: if `origin/main`'s tip tree carries no `tier full` receipt, run
# `make test-full` against that tip and let the ordinary `gate-receipt.sh`
# postlude certify it. That is the whole producer.
#
# WHY THE TRIGGER IS THE TREE'S OWN CERTIFICATION STATE, NOT A PATH DIFF.
# The story proposed running the browser tier for a tree whose diff touches
# `src/web_dashboard.html` or `e2e/`. A council rejected that unanimously
# (`story show SH-418` carries the verdict) on a soundness hole: a change to
# `src/web.rs` or `src/daemon/**` can break a browser spec with neither path
# in its diff, so a path predicate under-triggers by construction — and a
# hand-kept path list is the shape this project has already paid for five
# times (SH-136, SH-198, SH-258, SH-260/276, SH-360). "Has this exact content
# passed the browser suite" is derived from content instead: it re-arms on
# every merge whatever it touched, it is free to evaluate, and a burst of
# merges coalesces into one run against the newest tip rather than one run
# each.
#
# WHY ITS OWN WORKTREE AND ITS OWN LOCK, NOT `merge-watch.sh`'s PASS. Also the
# council's, and decided on a measurement: `main` took 11, 24, 16 and 9 merges
# on 2026-08-15..18, and the browser leg measured 1454s (24.2 minutes, four
# Playwright projects) on this machine. Folding that into `merge-watch.sh`'s
# 1-3 minute reconcile pass would starve SH-396's merge gate for hours a day.
# The lock is what stops a timer firing again mid-run and stacking a second
# 24-minute four-project Playwright suite on top of the first.
#
# THE TOOLCHAIN IS REFUSED, NEVER INSTALLED. `e2e/node_modules` is gitignored,
# so a fresh worktree has none and `run-e2e.sh` refuses. This script checks
# that BEFORE spending the Rust legs — `make test-full` runs the browser leg
# last, so an unprovisioned worktree would otherwise fail at the final line
# after ~10 minutes of work, which HARDENING_PROGRESS.md already records as a
# trap. It refuses rather than running `make e2e-install` itself: that is a
# network fetch writing outside the repo (a shared Playwright browser cache),
# and `make e2e-install`'s own header already takes the position that it is a
# per-machine bootstrap step to opt into.
#
# WHAT THIS SCRIPT DOES NOT DECIDE. The "should the browser tier run" question
# belongs entirely to `scripts/browser-status.sh`, which this calls and obeys.
# That is deliberate: `merge-watch.sh`'s header explains why thin GitHub
# orchestration carries no automated test, and the lesson taken from it here
# is to keep the *decision* out of the untestable layer. `tests/
# browser_gate.rs` covers the decision exhaustively against real git; what is
# left here is fetch, lock, checkout, exec.
#
# USAGE
#   browser-watch.sh [--plan] [--ref REF]
#
#   --plan      report the decision and the exact command a real pass would
#               run, then stop. The command is one array, printed here and
#               executed below, so this cannot drift from what runs.
#   --ref REF   ask about REF instead of `origin/main`. Suppresses the fetch,
#               which is what makes a test fixture possible without mocking.
#
# EXIT CODES
#   0   nothing to do (already certified, or another pass holds the lock), or
#       the browser tier ran and passed
#   1   the browser tier ran and FAILED — this tree is not release-ready
#   2   refused to run (no toolchain in the poller worktree, or a git failure)

set -uo pipefail

die() {
    printf 'browser-watch: %s\n' "$1" >&2
    exit 2
}

note() {
    printf 'browser-watch: %s\n' "$1" >&2
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
    *) die "unknown argument '$1' -- usage: browser-watch.sh [--plan] [--ref REF]" ;;
    esac
done

# The reader that holds this script's entire decision, resolved beside THIS
# file rather than under the repository being asked about. The two ship
# together, so a pass must use the sibling it was released with -- and it is
# what lets `tests/browser_gate.rs` point the tracked pair at a disposable
# fixture repository instead of mocking either of them.
script_dir="$(cd "$(dirname "$0")" && pwd)" || die "cannot resolve this script's directory"

root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"

# Only the default asks the network. An explicit --ref is answering about a
# ref the caller already has, which is also what lets `tests/browser_gate.rs`
# drive this against a real local repository with no remote at all.
if [ -z "$ref" ]; then
    ref="origin/main"
    note "fetching $ref"
    git fetch -q origin main || die "could not fetch origin/main"
fi

status_out="$(bash "$script_dir/browser-status.sh" "$ref" 2>/dev/null)"
status_code=$?
[ "$status_code" -le 2 ] || die "browser-status.sh could not answer for '$ref'"

tip="$(printf '%s\n' "$status_out" | sed -n 's/^tip=//p')"
tip_tree="$(printf '%s\n' "$status_out" | sed -n 's/^tip_tree=//p')"
state="$(printf '%s\n' "$status_out" | sed -n 's/^state=//p')"

# ONE array: printed by --plan, executed below. A test that pins what --plan
# prints therefore pins what a real pass runs, which is the only way to fence
# "this poller runs the FULL tier" without mocking make.
run_cmd=(make test-full)

if [ "$state" = "current" ]; then
    note "up to date — $ref's tree ($tip_tree) already carries a full receipt"
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

# A lock DIRECTORY, because mkdir(2) is the atomic primitive available on
# every platform this runs on (macOS ships no flock(1)). Staleness is decided
# by a fact -- whether the recorded pid is still alive -- and never by a
# timeout, which would be a bare literal about how long a browser suite is
# allowed to take.
lock="$common_dir/storyhook/browser-watch.lock"
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
printf '%s\n' "$$" > "$lock/pid"
trap 'rm -rf "$lock"' EXIT

worktree="$common_dir/storyhook/browser-watch-worktree"
if [ ! -e "$worktree/.git" ]; then
    note "creating the persistent browser-tier worktree at $worktree"
    git worktree add -q --detach "$worktree" "$tip" \
        || die "could not create the browser-tier worktree"
fi
git -C "$worktree" checkout -q --detach "$tip" \
    || die "could not check $tip out in $worktree"

if [ ! -d "$worktree/e2e/node_modules" ] \
    || ! (cd "$worktree/e2e" && npx --no-install playwright --version >/dev/null 2>&1); then
    note "$worktree has no e2e toolchain, so the browser tier cannot run there."
    note "  Provision it once:  (cd $worktree && make e2e-install)"
    note "  Refusing rather than installing it: that is a network fetch writing"
    note "  a shared browser cache outside this repo, which 'make e2e-install'"
    note "  already documents as a per-machine step to opt into."
    exit 2
fi

reports="$common_dir/storyhook/browser-watch-reports"
mkdir -p "$reports" || die "could not create $reports"
day="$(date -u +%Y-%m-%d)"
started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

note "running '${run_cmd[*]}' against ${tip:0:9} (tree $tip_tree) in $worktree"
(cd "$worktree" && "${run_cmd[@]}")
run_status=$?
finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Forensics, deliberately NOT a verdict. `docs/spec/test-tiers.md` already
# blesses this exact shape for the orphan reap (SH-412): a durable per-day log
# under the shared, never-committed, never-cloned git directory. It is not the
# marker file the SH-418 council declined -- nothing reads it to decide whether
# a tree is certified, and no gate consults it. Its whole job is that a pass
# which found a red suite leaves evidence behind after the terminal scrollback
# is gone.
printf '%s %s tip=%s tree=%s state_before=%s exit=%s\n' \
    "$started" "$finished" "$tip" "$tip_tree" "$state" "$run_status" \
    >> "$reports/$day.log"

if [ "$run_status" -eq 0 ]; then
    note "green — tree $tip_tree now carries a full receipt"
    exit 0
fi

note "RED — '${run_cmd[*]}' failed against ${tip:0:9}; this tree is not \
release-ready and no receipt was written. Logged to $reports/$day.log"
exit 1
