#!/usr/bin/env bash
#
# Print the NAME of origin's default branch, asked of origin itself.
#
#   origin-default-branch.sh
#
# `git ls-remote --symref origin HEAD` advertises the remote's own HEAD — one
# read-only round trip, no `gh`, any git host. On success the name (no
# `origin/`) is printed and nothing else; on failure nothing is printed, the
# reason goes to stderr, and the exit status is 1.
#
# WHY ORIGIN, NOT THE LOCAL CACHE, AND NEVER A LITERAL (SH-691). Git writes
# refs/remotes/origin/HEAD at clone time and no fetch refreshes it, so when
# this repository's default moved to `dev` every stale checkout kept
# answering `main`, and a helper that fell back to the literal `main` where
# the cache was absent answered the same. Five pull requests were opened,
# certified and merged on the wrong branch before anyone compared the
# branches by hand. The cache is a copy of a fact that has an authority
# (SH-136); a literal turns "I do not know" into a confident wrong answer
# (SH-394). A remote whose HEAD is unborn or detached advertises no `ref:`
# line at exit 0; that is absence, not an answer (SH-372), and is refused.
#
# This is the verifier bundle's copy of plugins/story/lib/session.sh's
# default_branch: the two ship separately (the bundle travels in the daemon
# binary, the plugin with a provider) and neither can source the other;
# tests/default_branch_contract.rs pins them to one answer.
#
# EXIT CODES
#   0   the name was printed
#   1   origin did not answer, or advertises no symbolic HEAD

set -uo pipefail

if ! out="$(git ls-remote --symref origin HEAD 2>&1)"; then
    printf 'origin-default-branch: origin did not answer: %s\n' "$out" >&2
    exit 1
fi
name="$(printf '%s\n' "$out" \
    | awk -F'\t' '$2 == "HEAD" && index($1, "ref: refs/heads/") == 1 { print substr($1, 17); exit }')"
if [ -z "$name" ]; then
    printf 'origin-default-branch: origin advertises no symbolic HEAD (its default branch is unborn or detached); set one on the remote, e.g. `gh repo edit --default-branch <name>`\n' >&2
    exit 1
fi
printf '%s\n' "$name"
