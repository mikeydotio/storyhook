#!/usr/bin/env bash
#
# The developer push gate's two phases — SH-306.
#
# `make test` is this project's entire CI (there is no push/PR job by policy),
# and until now the only thing enforcing "run it before you push" was a Claude
# Code `PreToolUse` hook with a 900-second timeout. That hook fires for every
# invocation shape, backgrounded ones included — but `make test` here is nine
# minutes nominal and routinely longer under the three-to-four concurrent
# worktree suites this machine actually runs. At 900s the harness SIGTERMs it
# and *lets the push proceed*: six of the eight hook logs left on that machine
# ended at exactly 900s across three days. A gate with a deadline converts a
# timeout into a pass.
#
# So the gate moved to git's own `pre-push`, which has no deadline and does not
# care how the shell was invoked. This script is the half that *earns* the
# right to push; `.githooks/pre-push` is the half that checks it.
#
# WHY A RECEIPT AND NOT A RE-RUN. A `pre-push` hook that ran `make test` would
# test the working tree while `git push` ships commits — on a dirty tree it
# certifies content that is not being pushed and says nothing about content
# that is. It would also start a second nine-minute suite while the first was
# still running, which this project has already recorded as the cause of a
# false red, and it would reintroduce the very deadline pressure that caused
# SH-306. So `make test` writes a receipt naming the *tree it tested*, and the
# hook asks whether the tree being pushed has one. The check costs
# milliseconds.
#
#   preflight  enrol this clone, refuse if the gate cannot work here, and
#              record the tree the suite is about to run against
#   postlude   the LAST line of `make test`, so make's own fail-fast semantics
#              mean it is reached only if every leg passed; certifies that tree
#
# The receipt is keyed on a tree object id — not a commit sha, and not a clock.
# Content is the thing that was tested; a clock is a second thing to be wrong
# about. Receipts live inside `.git/`, so one can never be committed and never
# be cloned: a fresh clone is fail-closed by construction.
#
# Forgery is deliberately not the threat model. Anyone able to hand-write a
# receipt already has `--no-verify` and `SKIP_PREPUSH_TESTS=1`, both sanctioned
# by CLAUDE.md. What this stops is the *silent, accidental* bypass — the one
# that already happened six times without anyone being told.

set -uo pipefail

phase="${1:-}"

die() {
    printf 'gate-receipt: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'gate-receipt: %s\n' "$1" >&2
}

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

# `--git-dir` is this worktree's PRIVATE directory (a linked worktree gets its
# own), which is where transient per-run state belongs: two worktrees running
# the suite at once must not read each other's preflight.
git_dir="$(cd "$(git rev-parse --git-dir)" && pwd)" \
    || die "cannot resolve this worktree's git directory"

# `--git-common-dir` is shared by every worktree, which is where receipts
# belong: the key is content, so two worktrees holding byte-identical trees
# genuinely did test the same thing.
common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"

receipts="$common_dir/storyhook/gate-receipts"
preflight_state="$git_dir/storyhook-gate-preflight"

# The tree object id of every TRACKED file as it currently stands in this
# worktree — HEAD's tree with any uncommitted edits to tracked files folded in.
# Untracked files are excluded on purpose: `target/`, `e2e/node_modules` and
# the suite's own scratch output are not what is being certified, and `git add
# -A` is forbidden here for the same reason.
tracked_tree() {
    local idx tree
    idx="$(mktemp -t storyhook-gate-index.XXXXXX)" || return 1
    rm -f "$idx"
    GIT_INDEX_FILE="$idx" git read-tree HEAD 2>/dev/null || { rm -f "$idx"; return 1; }
    GIT_INDEX_FILE="$idx" git add -u -- :/ 2>/dev/null || { rm -f "$idx"; return 1; }
    tree="$(GIT_INDEX_FILE="$idx" git write-tree 2>/dev/null)" || { rm -f "$idx"; return 1; }
    rm -f "$idx"
    [ -n "$tree" ] || return 1
    printf '%s\n' "$tree"
}

case "$phase" in
preflight)
    # 1. Enrol — idempotent, silent when already right, loud when it changes,
    #    and refusing rather than clobbering a value another tool owns.
    current="$(git config --local --get core.hooksPath 2>/dev/null || true)"
    if [ -z "$current" ]; then
        git config --local core.hooksPath .githooks \
            || die "could not set core.hooksPath"
        note "enrolled this clone — core.hooksPath is now .githooks, so git \
enforces the push gate from here on"
    elif [ "$current" != ".githooks" ]; then
        die "core.hooksPath is already set to '$current', which something else \
owns. Refusing to overwrite it. Either point it at .githooks yourself, or \
chain '$current' to .githooks/pre-push."
    fi

    # 2. The gate has to actually exist on the branch checked out HERE.
    #    `core.hooksPath` is relative, so each worktree runs its own branch's
    #    copy — which is the point, and also the hole: a branch cut before
    #    .githooks/ existed has no hook and is silently ungated.
    [ -x ".githooks/pre-push" ] \
        || die "this worktree has no executable .githooks/pre-push, so nothing \
is gating its pushes. It is on a branch that predates the gate — rebase onto \
main before pushing."

    # 3. Name the siblings this enrolment affects. core.hooksPath lives in the
    #    SHARED config, and setting it disables $GIT_DIR/hooks wholesale for
    #    every worktree at once — so any sibling on a pre-gate branch loses the
    #    storyhook-managed hooks too. Loud and enumerated beats silent.
    stale=""
    while read -r _ path; do
        [ -n "${path:-}" ] || continue
        [ "$path" = "$root" ] && continue
        [ -e "$path/.githooks/pre-push" ] && continue
        stale="$stale  $path
"
    done < <(git worktree list --porcelain 2>/dev/null | grep '^worktree ' || true)
    if [ -n "$stale" ]; then
        note "these worktrees are on branches that predate .githooks/, so they \
are ungated AND their storyhook-managed hooks are inactive until they rebase \
onto main:"
        printf '%s' "$stale" >&2
    fi

    tree="$(tracked_tree)" \
        || die "could not resolve this worktree's tracked content"
    printf '%s\n' "$tree" > "$preflight_state" \
        || die "could not record the preflight tree"
    ;;

postlude)
    [ -f "$preflight_state" ] \
        || die "no preflight was recorded — 'make test' must run \
'gate-receipt.sh preflight' before its first leg, or nothing certified is \
certified against anything"
    before="$(cat "$preflight_state")"
    after="$(tracked_tree)" \
        || die "could not resolve this worktree's tracked content"

    # Mid-run drift. Nine minutes is long enough for an agent to edit tracked
    # files while the suite runs, and a receipt written blind at the end would
    # certify a mixture no single run ever tested end to end.
    if [ "$before" != "$after" ]; then
        note "tracked content changed while the suite was running, so this run \
certifies nothing. Changed:"
        git diff --name-only "$before" "$after" 2>/dev/null | sed 's/^/  /' >&2
        die "run the gate again on the tree you intend to push"
    fi

    mkdir -p "$receipts" || die "could not create $receipts"
    # Atomic: the filename IS the claim, so a half-written one would read as
    # valid. rename(2) within a directory is atomic; a SIGTERM mid-write — the
    # exact failure this story measured — leaves the temp file, not a receipt.
    tmp="$receipts/.tmp.$$"
    {
        printf 'tree %s\n' "$after"
        printf 'worktree %s\n' "$root"
    } > "$tmp" || die "could not stage the receipt"
    mv -f "$tmp" "$receipts/$after" || die "could not publish the receipt"
    rm -f "$preflight_state"
    ;;

*)
    die "usage: gate-receipt.sh preflight|postlude"
    ;;
esac
