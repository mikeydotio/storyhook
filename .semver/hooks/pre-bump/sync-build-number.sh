#!/usr/bin/env bash
# Reserve the version change's build identity before the release commit.
set -euo pipefail

: "${OLD_VERSION:?sync-build-number: OLD_VERSION is required}"
: "${NEW_VERSION:?sync-build-number: NEW_VERSION is required}"
[ "$OLD_VERSION" != "$NEW_VERSION" ] || exit 0

# Keep allocator ownership through staging so another builder cannot replace
# the counter between reservation and the index snapshot. Failed attempts
# consume their reservation, just like failed builds for use.
python3 scripts/build-number.py -- git add -- BUILD
