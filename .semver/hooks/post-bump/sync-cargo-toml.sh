#!/usr/bin/env bash
# Post-bump hook: sync Cargo.toml version with VERSION file.
# Called by semver plugin after bumping. $NEW_VERSION has the "v" prefix.

set -euo pipefail

VERSION="${NEW_VERSION#v}"
sed -i "s/^version = \".*\"/version = \"${VERSION}\"/" Cargo.toml
