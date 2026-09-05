#!/usr/bin/env bash
# Build and verify the four archives uploaded by scripts/release.sh.
set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '    %s\n' "$*"; }

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
  || die "not inside a git repository"
# shellcheck source=scripts/release-targets.sh
source "$repo_root/scripts/release-targets.sh"
# shellcheck source=scripts/release-toolchain.sh
source "$repo_root/scripts/release-toolchain.sh"
toolchain_lock="$repo_root/scripts/release-toolchain.lock"
linux_runner="${STORYHOOK_RELEASE_LINUX_RUNNER:-$repo_root/scripts/release-linux.sh}"

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

[ "$(uname -s)" = "Darwin" ] \
  || die "local release assembly requires macOS for the Darwin targets"
case "$(uname -m)" in
  arm64|aarch64) host_arch="aarch64" ;;
  x86_64|amd64) host_arch="x86_64" ;;
  *) die "unsupported macOS release architecture $(uname -m)" ;;
esac
for tool in tar shasum file xcrun; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is required for local release assembly"
done
xcrun --find clang >/dev/null 2>&1 \
  || die "Xcode command-line tools are required for the Darwin targets"
[ -x "$linux_runner" ] || die "Linux release runner is not executable: $linux_runner"

target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
release_cache_root="${STORYHOOK_RELEASE_CACHE_DIR:-$target_dir/release-support}"
host_target="$host_arch-apple-darwin"
darwin_targets=(aarch64-apple-darwin x86_64-apple-darwin)
toolchain="$(ensure_release_toolchain "$host_target" "$release_cache_root" "$toolchain_lock" "${darwin_targets[@]}")" \
  || exit 1

# `--check` proves the outputs, not the presence of favored tool names. These
# link probes also warm the exact private toolchain reused by the build. The
# Linux runner independently requires its guest to report glibc 2.31.
probe_dir="$(mktemp -d /tmp/storyhook-release-probe.XXXXXX)"
printf 'fn main() {}\n' > "$probe_dir/main.rs"
for target in "${darwin_targets[@]}"; do
  "$toolchain/bin/rustc" --target "$target" -C linker=clang \
    "$probe_dir/main.rs" -o "$probe_dir/$target"
  description="$(LC_ALL=C file -b "$probe_dir/$target")"
  case "$target:$description" in
    x86_64-apple-darwin:*Mach-O*64-bit*x86_64*) ;;
    aarch64-apple-darwin:*Mach-O*64-bit*arm64*) ;;
    *) die "Darwin capability probe has the wrong format for $target: $description" ;;
  esac
done
rm -rf "$probe_dir"
"$linux_runner" --check

[ "$check_only" = 1 ] && exit 0

mkdir -p "$output_dir"
checksum_file="$output_dir/SHA256SUMS"
rm -f "$checksum_file"
for artifact in "${RELEASE_ARTIFACTS[@]}"; do
  rm -f "$output_dir/$artifact"
done

build_id="$("$repo_root/scripts/tracked-tree.sh" | cut -c1-12)"
[ -n "$build_id" ] || die "could not calculate the tracked-tree build identity"

for index in "${!RELEASE_TARGETS[@]}"; do
  target="${RELEASE_TARGETS[$index]}"
  builder="${RELEASE_BUILDERS[$index]}"
  artifact="${RELEASE_ARTIFACTS[$index]}"
  binary="$target_dir/$target/release/story"
  archive="$output_dir/$artifact"

  info "building $target with $builder"
  if [ "$dry_run" = 1 ]; then
    if [ "$builder" = "lima" ]; then
      info "[dry-run] Lima cargo build --locked --release --target $target (CARGO_PROFILE_RELEASE_STRIP=symbols)"
    else
      info "[dry-run] private cargo build --locked --release --target $target"
    fi
    info "[dry-run] package and verify $archive"
    continue
  fi

  if [ "$builder" = "lima" ]; then
    "$linux_runner" --target "$target" --output "$binary" --build-id "$build_id"
  else
    env \
      "PATH=$toolchain/bin:$PATH" \
      "RUSTC=$toolchain/bin/rustc" \
      "CARGO_TARGET_DIR=$target_dir" \
      "$toolchain/bin/cargo" build --locked --release --target "$target"
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
  case "$target" in
    *-unknown-linux-gnu)
      case "$description" in
        *,\ stripped) ;;
        *) die "$binary has the wrong symbol state for $target: $description" ;;
      esac
      ;;
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
