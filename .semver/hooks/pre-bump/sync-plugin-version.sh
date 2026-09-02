#!/usr/bin/env bash
# Pre-bump hook: give every plugin manifest the version the release is cutting,
# and stage them so they land in the atomic release commit.
#
# A direct sibling of sync-cargo-toml.sh, for the same reason and with the same
# timing: the semver plugin creates the release commit with no pathspec, so a
# pre-bump hook that edits *and stages* rides along in it. Run post-bump it
# would leave the manifests dirty in the working tree, one release behind.
#
# Why this exists (SH-530). The manifests said `0.6.0+codex.20260823221659`
# while Cargo.toml said `2.2.0`, and nothing required the two to move together:
# `tests/plugin_contract.rs` checked the three manifests agreed with EACH OTHER
# and never against the crate. Three files agreeing on a stale answer passed.
#
# That is not cosmetic. `src/plugin.rs` derives the Codex plugin cache path FROM
# the version string -- `~/.codex/plugins/cache/storyhook/story/<version>/` --
# so a version that never advances is a cache key that never advances, and every
# content change silently coalesces onto one directory. This is SH-406's lesson
# ("a version string is not a build identity") in a place where the string is
# load-bearing rather than merely reported.
#
# Env (set by the semver hook runner): $NEW_VERSION carries the "v" prefix; cwd
# is the project root. A non-zero exit aborts the bump, so a failed sync fails
# loud rather than shipping a release whose plugin claims another version.

set -euo pipefail

VERSION="${NEW_VERSION#v}"

# Derived from the tracked tree rather than hand-listed, so a manifest added
# later is picked up with no edit here -- the failure mode SH-136, SH-198,
# SH-258, SH-260/276, SH-360 and SH-364 each cost this project once already.
# `tests/version_identity.rs` derives the same set independently and fails if
# any of them disagrees with the crate version, so a manifest this hook misses
# is caught by the suite rather than shipped.
manifests="$(git ls-files -- '*.claude-plugin/*.json' '*.codex-plugin/*.json' '.agents/plugins/*.json')"

[ -n "$manifests" ] || {
    echo "sync-plugin-version: no plugin manifests found; refusing to claim a sync" >&2
    exit 1
}

for manifest in $manifests; do
    # Only a top-level or per-plugin `"version":` key, never a `$schema` URL or
    # a nested version-shaped string: anchored on the quoted key.
    sed -i.bak "s/\"version\"[[:space:]]*:[[:space:]]*\"[^\"]*\"/\"version\": \"${VERSION}\"/" \
        "$manifest" && rm -f "${manifest}.bak"
    git add -- "$manifest"
done
