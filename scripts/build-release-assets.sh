#!/usr/bin/env bash
# Build and verify the four archives uploaded by scripts/release.sh.
set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '    %s\n' "$*"; }

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
  || die "not inside a git repository"
# shellcheck source=scripts/release-targets.sh
source "$repo_root/scripts/release-targets.sh"

[ "${#RELEASE_TARGETS[@]}" -eq "${#RELEASE_BUILDERS[@]}" ] \
  && [ "${#RELEASE_TARGETS[@]}" -eq "${#RELEASE_ARTIFACTS[@]}" ] \
  || die "release target, builder, and artifact definitions are not index-aligned"

check_only=0
dry_run=0
version=""
output_dir=""

while [ $# -gt 0 ]; do
  case "$1" in
    --check) check_only=1; shift ;;
    --version) version="${2:-}"; [ -n "$version" ] || die "--version needs a value"; shift 2 ;;
    --output-dir) output_dir="${2:-}"; [ -n "$output_dir" ] || die "--output-dir needs a path"; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    *) die "unknown argument \`$1\`" ;;
  esac
done

if [ "$check_only" = 1 ] && { [ -n "$version" ] || [ -n "$output_dir" ] || [ "$dry_run" = 1 ]; }; then
  die "--check is a standalone preflight"
fi
if [ "$check_only" = 0 ]; then
  [ -n "$version" ] || die "--version is required"
  [ -n "$output_dir" ] || die "--output-dir is required"
  release_version_is_valid "$version" \
    || die "version must be vMAJOR.MINOR.PATCH, got \`$version\`"
fi

for tool in rustup cross docker tar shasum file; do
  command -v "$tool" >/dev/null 2>&1 \
    || die "$tool is required for local release assembly"
done

[ "$(uname -s)" = "Darwin" ] \
  || die "local release assembly requires macOS for the Darwin targets"
rustup run stable rustc --version >/dev/null 2>&1 \
  || die "the stable rustup toolchain is required; run \`rustup toolchain install stable\`"

installed_targets="$(rustup target list --toolchain stable --installed)"
for target in x86_64-apple-darwin aarch64-apple-darwin; do
  printf '%s\n' "$installed_targets" | grep -qx "$target" \
    || die "Rust target $target is required; run \`rustup target add --toolchain stable $target\`"
done

docker info >/dev/null 2>&1 \
  || die "the Docker engine is not running; start Docker before releasing"

[ "$check_only" = 1 ] && exit 0

target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
mkdir -p "$output_dir"
checksum_file="$output_dir/SHA256SUMS"
rm -f "$checksum_file"
for artifact in "${RELEASE_ARTIFACTS[@]}"; do
  rm -f "$output_dir/$artifact"
done

for index in "${!RELEASE_TARGETS[@]}"; do
  target="${RELEASE_TARGETS[$index]}"
  builder="${RELEASE_BUILDERS[$index]}"
  artifact="${RELEASE_ARTIFACTS[$index]}"
  binary="$target_dir/$target/release/story"
  archive="$output_dir/$artifact"

  info "building $target with $builder"
  if [ "$dry_run" = 1 ]; then
    if [ "$builder" = "cross" ]; then
      info "[dry-run] cross +stable build --locked --release --target $target"
    else
      info "[dry-run] rustup run stable cargo build --locked --release --target $target"
    fi
    info "[dry-run] package and verify $archive"
    continue
  fi

  if [ "$builder" = "cross" ]; then
    cross +stable build --locked --release --target "$target"
  else
    rustup run stable cargo build --locked --release --target "$target"
  fi
  [ -x "$binary" ] || die "$builder did not produce an executable at $binary"

  description="$(LC_ALL=C file -b "$binary")"
  case "$target:$description" in
    x86_64-unknown-linux-gnu:*ELF*64-bit*x86-64*) ;;
    aarch64-unknown-linux-gnu:*ELF*64-bit*ARM*aarch64*) ;;
    x86_64-apple-darwin:*Mach-O*64-bit*x86_64*) ;;
    aarch64-apple-darwin:*Mach-O*64-bit*arm64*) ;;
    *) die "$binary has the wrong format for $target: $description" ;;
  esac

  tar -C "$(dirname "$binary")" -czf "$archive" story
  contents="$(tar -tzf "$archive")"
  [ "$contents" = "story" ] \
    || die "$archive must contain exactly one top-level story executable; found: $contents"
  digest="$(shasum -a 256 "$archive" | awk '{print $1}')"
  [ "${#digest}" -eq 64 ] || die "could not calculate SHA-256 for $archive"
  printf '%s  %s\n' "$digest" "$artifact" >> "$checksum_file"
  info "verified $artifact sha256:$digest"
done

if [ "$dry_run" = 0 ]; then
  [ "$(wc -l < "$checksum_file" | tr -d ' ')" -eq "${#RELEASE_ARTIFACTS[@]}" ] \
    || die "the checksum manifest is incomplete"
  info "checksums $checksum_file"
fi
