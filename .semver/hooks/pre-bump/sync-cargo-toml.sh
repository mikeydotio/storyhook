#!/usr/bin/env bash
# Pre-bump hook: sync the Rust package version (Cargo.toml + Cargo.lock) with the
# VERSION file, and stage both so they land in the atomic release commit.
#
# Why pre-bump, not post-bump: the semver plugin creates the release commit with
# no pathspec (`git commit -m ...`), so it captures the whole staged index. A
# pre-bump hook that edits *and stages* Cargo.toml/Cargo.lock therefore rides
# along in that commit. Run as a post-bump hook it edited Cargo.toml *after* the
# commit, leaving it — and a now-stale Cargo.lock — dirty in the working tree.
#
# Env (set by the semver hook runner): $NEW_VERSION carries the "v" prefix; cwd
# is the project root. A non-zero exit aborts the bump (pre-bump semantics), so
# a failed sync fails loud instead of shipping a mismatched release.

set -euo pipefail

VERSION="${NEW_VERSION#v}"

# --- Cargo.toml: the [package] version line ---
# Anchored at column 0, so dependency version keys (indented / inline) are never
# touched. Portable in-place edit: bare `sed -i` is GNU-only and fails on
# BSD/macOS sed; `-i.bak` works on both — delete the backup afterward.
sed -i.bak "s/^version = \".*\"/version = \"${VERSION}\"/" Cargo.toml && rm -f Cargo.toml.bak
git add -- Cargo.toml

# --- Cargo.lock: this package's own version entry ---
# In Cargo.lock the `version` line immediately follows its package's `name` line.
# Rewriting just that one line is exactly what cargo does when the root package's
# version changes, so the lockfile stays consistent and the next `cargo build`
# won't re-dirty it. The package name is read from Cargo.toml's [package] table so
# this hook stays reusable across Rust projects.
pkg="$(awk '
  /^\[package\]/ { in_pkg = 1; next }
  /^\[/          { in_pkg = 0 }
  in_pkg && /^name[[:space:]]*=/ {
    gsub(/^name[[:space:]]*=[[:space:]]*"|"[[:space:]]*$/, "")
    print; exit
  }
' Cargo.toml)"

if [ -n "$pkg" ] && [ -f Cargo.lock ]; then
  awk -v pkg="$pkg" -v v="$VERSION" '
    prev_was_pkg { sub(/^version = ".*"/, "version = \"" v "\""); prev_was_pkg = 0 }
    { print }
    $0 == "name = \"" pkg "\"" { prev_was_pkg = 1 }
  ' Cargo.lock > Cargo.lock.tmp && mv Cargo.lock.tmp Cargo.lock
  git add -- Cargo.lock
fi
