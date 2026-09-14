#!/usr/bin/env bash
#
# The developer push gate's two phases — SH-306.
#
# `make test` (or `make test-full` -- SH-394) is this project's entire CI
# (there is no push/PR job by policy), and until now the only thing enforcing
# "run it before you push" was a Claude
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
#   preflight       enrol this clone, refuse if the gate cannot work here, and
#                   record the tree the suite is about to run against
#   postlude [TIER] the LAST line of `make test` / `make test-full`, so make's
#                   own fail-fast semantics mean it is reached only if every
#                   leg passed; certifies that tree
#
# TIER is `changed` (SH-429 -- a selective run over `scripts/select-tests.sh`'s
# own pick of test binaries), `gate` (the default -- fmt, clippy, the whole
# Rust suite, the plugin bash harness) or `full` (`gate` plus the browser
# suite -- SH-394). It names what was actually run, nothing more:
# `.githooks/pre-push` accepts any of the three for a push, because the
# reduced gate protecting `main` is the whole point of the split, but it
# reports which one a push carries so that is never silent. The three form a
# strict order, `changed < gate < full`, and postlude never lets a weaker-tier
# run overwrite a stronger receipt already on file for the same tree --
# re-running a cheap tier after an expensive one must not erase the stronger
# claim. `scripts/merge-preflight.sh` -- unlike the push hook -- does NOT
# accept `changed`: a merge tree is content no single branch's selective run
# ever accounted for (SH-396's own reason for existing), so landing on `main`
# still requires `gate` or `full` regardless of what tier a push carried (a
# council verdict, recorded on story SH-429).
#
# A `changed` receipt additionally carries a fourth line, `base <tree>`,
# naming the fully-certified (`gate`/`full`) tree `select-tests.sh` diffed
# against to decide what to run -- the fact that makes the claim honest: "the
# selected tests passed, relative to a specific prior green tree," never "the
# whole suite passed." `postlude` refuses to write one without a `base` that
# itself currently carries a `gate`/`full` receipt.
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

# Receipt mechanics live in tree-receipt.sh (SH-665). Keep this command as
# the project's compatibility entry point: local gates still enroll hooks.
set -uo pipefail

die() {
    printf 'gate-receipt: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'gate-receipt: %s\n' "$1" >&2
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" \
    || die "cannot resolve the receipt script directory"
root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

if [ "${1:-}" = preflight ]; then
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

fi

exec bash "$script_dir/tree-receipt.sh" "$@"
