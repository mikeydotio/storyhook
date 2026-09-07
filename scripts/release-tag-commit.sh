#!/usr/bin/env bash
#
# Which commit a release tag belongs on — SH-494.
#
# `scripts/release.sh` used to run `git tag -a "$version"` with no commit
# argument, after `gh pr merge --merge` and `git pull --ff-only`. That tags
# HEAD, which at that moment is the MERGE commit; the commit that actually
# modified `VERSION` is its parent, over on the release branch. `semver-cli`'s
# `tag_correct_commit` check compares the tag against `git log -1 -- VERSION`,
# so the two never agreed, and because that consent check runs against the
# CURRENT version, every release cut this way refused the NEXT one before it
# started. v2.2.0 shipped fine and v2.3.0 could not begin.
#
# WHY THIS ASKS THE QUESTION RATHER THAN CARRYING AN ANSWER. The obvious repair
# is to capture `git rev-parse HEAD` on the release branch and pass it to
# `git tag` later. That works, and it is still a second opinion: it would be
# release.sh's belief about which commit made the version, sitting beside
# semver's own derivation of the same fact, free to drift the moment either
# side changes. Asking `git log -1 -- VERSION` — the identical query semver
# asks — makes agreement structural rather than maintained. This is the
# doctrine SH-482 applied to dispatch's claim one layer down: delete the second
# opinion, do not synchronise it.
#
# CONTRACT, deliberately `scripts/tracked-tree.sh`'s (SH-406): stdout carries
# the commit oid and nothing else, and a nonzero exit means "no answer" — never
# a plausible-looking oid a caller would go on to tag. A refusal writes to
# stderr, so a caller using `$(...)` gets an empty string and a failed status
# rather than a sentence where an oid should be.
#
#   Usage: release-tag-commit.sh <version> [revision]
# The optional revision scopes historical tag audits; ordinary release callers
# retain HEAD as their default. Resolve it to a commit before passing it to log.
#
# <version> is required and is checked rather than trusted: the commit this
# names must itself contain a `VERSION` reading exactly that. A caller asking
# about a version the tree does not carry is mid-release or on the wrong
# branch, and answering would mint precisely the wrong pointer this exists to
# prevent.

set -uo pipefail

die() {
    printf 'release-tag-commit: %s\n' "$1" >&2
    exit 1
}

version="${1:-}"
[ -n "$version" ] \
    || die "usage: release-tag-commit.sh <version> — refusing to guess which version is meant"
[ "$#" -le 2 ] || die "usage: release-tag-commit.sh <version> [revision]"

git rev-parse --git-dir >/dev/null 2>&1 \
    || die "not inside a git repository"

# The one query, and the whole mechanism: git's history simplification omits a
# merge whose tree matches a parent's, so this walks past the merge commit to
# the release commit underneath it, exactly as semver does.
revision="$(git rev-parse --verify --end-of-options "${2:-HEAD}^{commit}" 2>/dev/null)" \
    || die "revision ${2:-HEAD} is not a commit"
commit="$(git log -1 --format=%H "$revision" -- VERSION 2>/dev/null)"
[ -n "$commit" ] \
    || die "no commit in this history modifies VERSION — there is nothing to tag for $version"

# What that commit actually says, read from the commit rather than the working
# tree: an uncommitted edit must not be able to talk this into an answer.
recorded="$(git show "$commit:VERSION" 2>/dev/null | tr -d '[:space:]')"
[ -n "$recorded" ] \
    || die "commit ${commit} carries no readable VERSION"

if [ "$recorded" != "$version" ]; then
    die "asked for $version, but the last commit to touch VERSION (${commit}) reads $recorded \
— refusing to tag $version onto a commit that does not carry it"
fi

printf '%s\n' "$commit"
