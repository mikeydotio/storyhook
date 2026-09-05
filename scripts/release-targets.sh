#!/usr/bin/env bash
# Shared release target definition. Keep the three arrays index-aligned.
# This file exports data to sourcing scripts.
# shellcheck disable=SC2034

RELEASE_TARGETS=(
  "x86_64-unknown-linux-gnu"
  "aarch64-unknown-linux-gnu"
  "x86_64-apple-darwin"
  "aarch64-apple-darwin"
)

RELEASE_BUILDERS=(
  "cross"
  "cross"
  "cargo"
  "cargo"
)

RELEASE_ARTIFACTS=(
  "story-x86_64-unknown-linux-gnu.tar.gz"
  "story-aarch64-unknown-linux-gnu.tar.gz"
  "story-x86_64-apple-darwin.tar.gz"
  "story-aarch64-apple-darwin.tar.gz"
)

release_version_is_valid() {
  printf '%s\n' "$1" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'
}
