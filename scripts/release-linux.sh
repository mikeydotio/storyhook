#!/usr/bin/env bash
# Build the GNU/Linux release targets in a glibc-2.31 Lima guest.
set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '    %s\n' "$*"; }

script_dir="$(cd "$(dirname "$0")" && pwd)"
lock_file="$script_dir/release-toolchain.lock"
# shellcheck source=scripts/release-toolchain.sh
source "$script_dir/release-toolchain.sh"

normalize_arch() {
  case "$1" in
    arm64|aarch64) printf 'aarch64\n' ;;
    x86_64|amd64) printf 'x86_64\n' ;;
    *) die "unsupported release architecture $1" ;;
  esac
}

linker_for_target() {
  local host_target="$1"
  local target="$2"
  if [ "$target" = "$host_target" ]; then
    printf 'cc\n'
  else
    case "$target" in
      aarch64-unknown-linux-gnu) printf 'aarch64-linux-gnu-gcc\n' ;;
      x86_64-unknown-linux-gnu) printf 'x86_64-linux-gnu-gcc\n' ;;
      *) die "unsupported Linux release target $target" ;;
    esac
  fi
}

# Resolve a relative guest cache root against the guest's HOME, never its
# working directory (SH-578).
#
# `limactl shell` does not start in the guest home. It starts in the host's
# working directory when that path is mounted, and otherwise falls back to the
# host home — which Lima's default template mounts READ-ONLY. This checkout
# lives outside every mount, so `$PWD` in the guest was `/Users/mikey`, and a
# cache root under it could not be created at all. `$HOME` is the guest's own,
# writable, and does not depend on where limactl chose to put us.
guest_absolute_cache_root() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s\n' "$HOME/$1" ;;
  esac
}

guest_check() {
  local host_target="$1"
  local guest_cache_root="$2"
  local toolchain target linker probe_dir description
  local targets=(aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu)

  guest_cache_root="$(guest_absolute_cache_root "$guest_cache_root")"

  toolchain="$(ensure_release_toolchain "$host_target" "$guest_cache_root" "$lock_file" "${targets[@]}")" \
    || exit 1
  probe_dir="$(mktemp -d /tmp/storyhook-release-probe.XXXXXX)"
  printf 'fn main() {}\n' > "$probe_dir/main.rs"
  for target in "${targets[@]}"; do
    linker="$(linker_for_target "$host_target" "$target")"
    command -v "$linker" >/dev/null 2>&1 \
      || die "Linux linker $linker is unavailable for $target"
    "$toolchain/bin/rustc" \
      --target "$target" \
      -C "linker=$linker" \
      -C strip=symbols \
      "$probe_dir/main.rs" \
      -o "$probe_dir/$target"
    description="$(LC_ALL=C file -b "$probe_dir/$target")"
    case "$target:$description" in
      x86_64-unknown-linux-gnu:*ELF*64-bit*x86-64*,\ stripped) ;;
      aarch64-unknown-linux-gnu:*ELF*64-bit*ARM*aarch64*,\ stripped) ;;
      *) die "Linux capability probe has the wrong format for $target: $description" ;;
    esac
  done
  rm -rf "$probe_dir"
}

guest_build() {
  local host_target="$1"
  local guest_cache_root="$2"
  local target="$3"
  local source_archive="$4"
  local output="$5"
  local build_id="$6"
  local toolchain linker linker_env_target work_dir binary
  local targets=(aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu)

  guest_cache_root="$(guest_absolute_cache_root "$guest_cache_root")"

  toolchain="$(ensure_release_toolchain "$host_target" "$guest_cache_root" "$lock_file" "${targets[@]}")" \
    || exit 1
  linker="$(linker_for_target "$host_target" "$target")"
  linker_env_target="$(printf '%s' "$target" | tr '[:lower:]-' '[:upper:]_')"
  command -v "$linker" >/dev/null 2>&1 || die "Linux linker $linker is unavailable for $target"
  work_dir="$(mktemp -d /tmp/storyhook-release-build.XXXXXX)"
  tar -xzf "$source_archive" -C "$work_dir"

  info "building $target in Lima (glibc 2.31, linker $linker)"
  env \
    "PATH=$toolchain/bin:$PATH" \
    "RUSTC=$toolchain/bin/rustc" \
    "CARGO_HOME=$guest_cache_root/cargo" \
    "CARGO_TARGET_DIR=$work_dir/target" \
    "CARGO_TARGET_${linker_env_target}_LINKER=$linker" \
    CARGO_PROFILE_RELEASE_STRIP=symbols \
    "STORYHOOK_BUILD_ID=$build_id" \
    "$toolchain/bin/cargo" build --locked --release --target "$target"
  binary="$work_dir/target/$target/release/story"
  [ -x "$binary" ] || die "Lima cargo did not produce an executable at $binary"
  mkdir -p "$(dirname "$output")"
  cp "$binary" "$output"
  rm -rf "$work_dir"
}

if [ "${1:-}" = "--guest-check" ]; then
  [ "$#" -eq 3 ] || die "guest check needs host target and cache root"
  guest_check "$2" "$3"
  exit 0
fi
if [ "${1:-}" = "--guest-build" ]; then
  [ "$#" -eq 7 ] || die "guest build needs host target, cache, target, source, output, and build id"
  guest_build "$2" "$3" "$4" "$5" "$6" "$7"
  exit 0
fi

check_only=0
target=""
output=""
build_id=""
while [ $# -gt 0 ]; do
  case "$1" in
    --check) check_only=1; shift ;;
    --target) target="${2:-}"; shift 2 ;;
    --output) output="${2:-}"; shift 2 ;;
    --build-id) build_id="${2:-}"; shift 2 ;;
    *) die "unknown argument \`$1\`" ;;
  esac
done

if [ "$check_only" = 1 ] && { [ -n "$target" ] || [ -n "$output" ] || [ -n "$build_id" ]; }; then
  die "--check is standalone"
fi
if [ "$check_only" = 0 ]; then
  [ -n "$target" ] || die "--target is required"
  [ -n "$output" ] || die "--output is required"
  [ -n "$build_id" ] || die "--build-id is required"
fi

for tool in limactl file tar shasum; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is required for Linux release assembly"
done

host_arch="$(normalize_arch "$(uname -m)")"
host_target="$host_arch-unknown-linux-gnu"
instance="storyhook-release-focal-$host_arch"
instance_json="$(limactl list "$instance" --json 2>/dev/null || true)"
if [ -z "$instance_json" ]; then
  info "creating Lima instance $instance"
  limactl start --tty=false --name="$instance" "$script_dir/release-linux.yaml"
else
  limactl start --tty=false "$instance"
fi

guest_arch="$(normalize_arch "$(limactl shell --tty=false "$instance" uname -m)")"
[ "$guest_arch" = "$host_arch" ] \
  || die "Lima guest architecture $guest_arch does not match host architecture $host_arch"
guest_glibc="$(limactl shell --tty=false "$instance" getconf GNU_LIBC_VERSION)"
[ "$guest_glibc" = "glibc 2.31" ] \
  || die "Lima guest must provide glibc 2.31; found $guest_glibc"

transfer_root="/tmp/storyhook-release-run-$$"
limactl shell --tty=false "$instance" mkdir -p "$transfer_root/scripts"
limactl copy --backend=scp "$script_dir/release-linux.sh" "$script_dir/release-toolchain.sh" "$lock_file" "$instance:$transfer_root/scripts/"
guest_runner="$transfer_root/scripts/release-linux.sh"
guest_cache_root=".cache/storyhook-release"

if [ "$check_only" = 1 ]; then
  limactl shell --tty=false "$instance" bash "$guest_runner" --guest-check "$host_target" "$guest_cache_root"
  limactl shell --tty=false "$instance" rm -rf "$transfer_root"
  exit 0
fi

scratch_dir="$(mktemp -d /tmp/storyhook-release-source.XXXXXX)"
source_archive="$scratch_dir/source.tar.gz"
git -C "$(git rev-parse --show-toplevel)" archive --format=tar.gz --output="$source_archive" HEAD
limactl copy --backend=scp "$source_archive" "$instance:$transfer_root/source.tar.gz"
guest_output="$transfer_root/story"
limactl shell --tty=false "$instance" bash "$guest_runner" --guest-build \
  "$host_target" "$guest_cache_root" "$target" "$transfer_root/source.tar.gz" "$guest_output" "$build_id"
mkdir -p "$(dirname "$output")"
limactl copy --backend=scp "$instance:$guest_output" "$output"
chmod +x "$output"
limactl shell --tty=false "$instance" rm -rf "$transfer_root"
rm -rf "$scratch_dir"
