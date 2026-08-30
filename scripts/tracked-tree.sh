#!/usr/bin/env bash
#
# Prints the git tree object id of every TRACKED file as it currently stands
# in this worktree -- HEAD's tree with any uncommitted edits to tracked files
# folded in. Untracked files are excluded on purpose: `target/`,
# `e2e/node_modules` and any scratch output are not part of the identity, and
# `git add -A` is deliberately never used here.
#
# Extracted from `scripts/gate-receipt.sh` (SH-306) when SH-406 gave the
# primitive a second caller, `build.rs`, which stamps every `story` binary
# with this value so two builds share a version string iff their tracked
# content is byte-identical (the SH-404 incident: two builds, one VERSION,
# different schemas understood, and nothing could tell them apart).
#
# Council verdict (SH-406, 2026-08-18, unanimous 3-0): BOTH callers invoke
# this file as a subprocess -- never `source` it. Two round-1 proposals
# independently suggested `source`-ing this into `gate-receipt.sh` while
# having `build.rs` invoke it directly, without noticing those are two
# different calling conventions for one function; the winning proposal
# collapsed both callers onto one contract instead: stdout carries the oid,
# a nonzero exit means "no answer," and that is the entire interface. This
# also means the script is provable in isolation with a real-git black-box
# test, the same style `tests/push_gate.rs` already uses for
# `gate-receipt.sh` itself, rather than a sourcing relationship a test would
# have to pin by static inspection instead of by running it.
#
# Exit contract: prints the oid (one line) and exits 0 on success. Prints
# nothing and exits nonzero on any failure -- no `.git` (a tarball, a
# `cargo install` source extraction), a corrupt index, or any other git
# error. A caller that must never fail on this file's behalf (`build.rs`)
# reads a nonzero exit as "no stamp available," not as an error to propagate.
#
# A caller that needs to resolve a generated dirty-tree oid after this process
# exits may pass one absolute, existing object directory that physically
# resides outside the source repository's object database. The caller owns
# that directory and its lifetime. With no argument this script owns a
# temporary object directory and removes it before returning. In both forms
# the source repository's canonical object database, plus any read-only
# alternates the caller needs to resolve HEAD, remain alternates: identity
# generation never inserts objects into the checkout it is inspecting.
#
# Measured cost on this repo (2026-08-18, warm, this checkout's tree size):
# ~130ms per invocation -- three git subprocess spawns plus a full
# tracked-file walk via `git add -u`. `build.rs` pays this whenever
# cargo's default no-`rerun-if-*` policy reruns the build script, which is
# broader than "on a release build": any changed file in the crate can
# trigger it, including under an editor's `cargo check` on save. Measured
# and accepted rather than assumed (this project's own SH-306 doctrine) --
# sub-150ms is a small fraction of any `cargo check`/`build`'s own wall
# clock, but it is not free, and a future slowdown here should be remeasured
# rather than reasoned about from this comment alone.

set -uo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 1
cd "$root" || exit 1

common_dir="$(cd "$(git rev-parse --git-common-dir 2>/dev/null)" && pwd -P)" \
    || exit 1
source_objects="$(cd "$common_dir/objects" && pwd -P)" || exit 1
inherited_alternates="${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}"
source_alternates="$source_objects"
if [ -n "$inherited_alternates" ]; then
    source_alternates="$source_alternates:$inherited_alternates"
fi

objects=""
owned_objects=0
idx=""

cleanup() {
    if [ -n "$idx" ]; then
        rm -f "$idx"
        idx=""
    fi
    if [ "$owned_objects" -eq 1 ] && [ -n "$objects" ]; then
        rm -rf "$objects"
        objects=""
        owned_objects=0
    fi
}

on_signal() {
    status="$1"
    trap - EXIT HUP INT TERM
    cleanup
    exit "$status"
}

trap cleanup EXIT
trap 'on_signal 129' HUP
trap 'on_signal 130' INT
trap 'on_signal 143' TERM

case "$#" in
0)
    objects="$(mktemp -d -t storyhook-tracked-tree-objects.XXXXXX)" || exit 1
    owned_objects=1
    ;;
1)
    case "$1" in
    /*) ;;
    *) exit 1 ;;
    esac
    [ -d "$1" ] || exit 1
    objects="$(cd "$1" && pwd -P)" || exit 1
    case "$objects/" in
    "$source_objects/"*)
        printf '%s\n' \
            'tracked-tree.sh: caller-owned object directory must be outside the source object store' \
            >&2
        exit 1
        ;;
    esac
    ;;
*)
    exit 1
    ;;
esac

idx="$(mktemp -t storyhook-tracked-tree-index.XXXXXX)" || exit 1
rm -f "$idx"

git_private() {
    GIT_INDEX_FILE="$idx" \
        GIT_OBJECT_DIRECTORY="$objects" \
        GIT_ALTERNATE_OBJECT_DIRECTORIES="$source_alternates" \
        git "$@"
}

git_private read-tree HEAD 2>/dev/null || exit 1
git_private add -u 2>/dev/null || exit 1
tree="$(git_private write-tree 2>/dev/null)" || exit 1

[ -n "$tree" ] || exit 1
printf '%s\n' "$tree"
