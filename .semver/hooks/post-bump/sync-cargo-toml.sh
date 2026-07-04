#!/usr/bin/env bash
# Post-bump hook: sync Cargo.toml version with VERSION file.
# Called by semver plugin after bumping. $NEW_VERSION has the "v" prefix.

set -euo pipefail

VERSION="${NEW_VERSION#v}"
# Portable in-place edit: `sed -i` without a suffix is GNU-only and fails on
# BSD/macOS sed. `-i.bak` works on both; delete the backup afterward.
sed -i.bak "s/^version = \".*\"/version = \"${VERSION}\"/" Cargo.toml && rm -f Cargo.toml.bak
