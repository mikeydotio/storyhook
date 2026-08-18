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
# Measured cost on this repo (2026-08-18, warm, this checkout's tree size):
# ~130ms per invocation -- three git subprocess spawns plus a full
# tracked-file walk via `git add -u -- :/`. `build.rs` pays this whenever
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

idx="$(mktemp -t storyhook-tracked-tree.XXXXXX)" || exit 1
rm -f "$idx"
GIT_INDEX_FILE="$idx" git read-tree HEAD 2>/dev/null || { rm -f "$idx"; exit 1; }
GIT_INDEX_FILE="$idx" git add -u -- :/ 2>/dev/null || { rm -f "$idx"; exit 1; }
tree="$(GIT_INDEX_FILE="$idx" git write-tree 2>/dev/null)" || { rm -f "$idx"; exit 1; }
rm -f "$idx"

[ -n "$tree" ] || exit 1
printf '%s\n' "$tree"
