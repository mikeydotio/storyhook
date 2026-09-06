#!/usr/bin/env bash
# Assemble an upstream Rust toolchain from the components pinned for releases.

release_toolchain_die() {
  printf 'error: %s\n' "$*" >&2
  return 1
}

release_toolchain_row() {
  local component="$1"
  local target="$2"
  local lock_file="$3"

  awk -F '\t' -v component="$component" -v target="$target" '
    $1 == component && $2 == target { print; found++ }
    END { if (found != 1) exit 1 }
  ' "$lock_file"
}

release_toolchain_install_component() {
  local component="$1"
  local target="$2"
  local prefix="$3"
  local download_dir="$4"
  local lock_file="$5"
  local row url expected_hash archive actual_hash unpack_dir package_dir

  row="$(release_toolchain_row "$component" "$target" "$lock_file")" \
    || release_toolchain_die "release toolchain lock needs exactly one $component row for $target" \
    || return
  IFS=$'\t' read -r _ _ url expected_hash <<< "$row"
  archive="$download_dir/$expected_hash.tar.gz"

  if [ -f "$archive" ]; then
    actual_hash="$(shasum -a 256 "$archive" | awk '{print $1}')"
    [ "$actual_hash" = "$expected_hash" ] \
      || release_toolchain_die "cached $component for $target failed SHA-256: expected $expected_hash, got $actual_hash ($archive)" \
      || return
  else
    command -v curl >/dev/null 2>&1 \
      || release_toolchain_die "curl is required while release toolchain component $component for $target is not cached" \
      || return
    local partial="$archive.partial.$$"
    curl --fail --location --silent --show-error "$url" --output "$partial" \
      || release_toolchain_die "could not download $component for $target from $url" \
      || return
    actual_hash="$(shasum -a 256 "$partial" | awk '{print $1}')"
    if [ "$actual_hash" != "$expected_hash" ]; then
      rm -f "$partial"
      release_toolchain_die "downloaded $component for $target failed SHA-256: expected $expected_hash, got $actual_hash"
      return
    fi
    mv "$partial" "$archive"
  fi

  unpack_dir="$(mktemp -d /tmp/storyhook-rust-component.XXXXXX)"
  tar -xzf "$archive" -C "$unpack_dir"
  package_dir="$(find "$unpack_dir" -mindepth 1 -maxdepth 1 -type d -print -quit)"
  [ -n "$package_dir" ] && [ -x "$package_dir/install.sh" ] \
    || release_toolchain_die "$component archive for $target has no executable installer" \
    || return
  "$package_dir/install.sh" \
    --prefix="$prefix" \
    --bindir="$prefix/bin" \
    --libdir="$prefix/lib" \
    --sysconfdir="$prefix/etc" \
    --datadir="$prefix/share" \
    --docdir="$prefix/share/doc" \
    --mandir="$prefix/share/man" \
    --disable-ldconfig \
    >/dev/null
  rm -rf "$unpack_dir"
}

# Where a pinned toolchain is assembled for a given host.
#
# The final component is deliberately NOT the host triple, and that is the
# whole point of this function existing rather than the path being spelled
# inline at its one call site (SH-576). rustc does not take its sysroot from
# its own argv0; it takes it from the location of the `librustc_driver` dylib
# it loaded, and it decides how far to walk up by reading the directory names
# it finds there. A dylib sitting in `<anything>/<target-triple>/lib/` is
# indistinguishable, by name, from one in `$sysroot/lib/rustlib/$target/lib/`
# — the layout rustc ships pre-built target libraries in — so rustc strips
# four components instead of one and lands on `$cache_root`. Every compile
# then fails with E0463 "can't find crate for `std`", advising a
# `rustup target add` that has nothing to do with this path.
#
# Keying the cache by host is still required, so the triple stays in the path
# and a leaf that cannot be read as a triple goes underneath it.
release_toolchain_root_path() {
  local cache_root="$1"
  local lock_hash="$2"
  local host_target="$3"

  printf '%s\n' "$cache_root/toolchains/$lock_hash/$host_target/toolchain"
}

# Prove the assembled toolchain finds its own sysroot, rather than assuming it.
#
# The check costs one `rustc --print sysroot` and buys the difference between
# a refusal that names its cause and rustc's own E0463, which misdiagnoses
# this as a missing rustup target. Both sides are resolved to physical paths
# first because rustc canonicalizes the answer it reports and the prefix it
# was handed may reach the same directory through a symlink.
release_toolchain_verify_sysroot() {
  local prefix="$1"
  local reported resolved_prefix resolved_reported

  reported="$("$prefix/bin/rustc" --print sysroot 2>/dev/null)" \
    || release_toolchain_die "release rustc at $prefix/bin/rustc could not report its sysroot" \
    || return
  # An empty answer is its own failure and is reported as one. `cd ""` succeeds
  # and stays put, so resolving it would name the caller's working directory as
  # though rustc had claimed it.
  [ -n "$reported" ] \
    || release_toolchain_die "release rustc at $prefix/bin/rustc reported no sysroot at all" \
    || return
  resolved_prefix="$(cd "$prefix" 2>/dev/null && pwd -P)" \
    || release_toolchain_die "release toolchain prefix $prefix is not a directory" \
    || return
  resolved_reported="$(cd "$reported" 2>/dev/null && pwd -P)" || resolved_reported="$reported"

  [ "$resolved_reported" = "$resolved_prefix" ] || {
    release_toolchain_die "release rustc auto-detects its sysroot as $resolved_reported, not $resolved_prefix.
    rustc reads the sysroot off the directory holding its librustc_driver
    dylib, so a toolchain whose own directory is named like a target triple
    is walked up too far and every compile fails with E0463. Assemble it
    somewhere whose final path component is not a target triple."
    return
  }
}

ensure_release_toolchain() {
  local host_target="$1"
  local cache_root="$2"
  local lock_file="$3"
  shift 3
  local required_targets=("$@")

  # One exit, so that no branch below can hand back a toolchain that was never
  # verified. The three ways of resolving one — an operator's override, a warm
  # cache, a fresh assembly — differ in everything except that obligation.
  local resolved=""

  if [ -n "${STORYHOOK_RELEASE_TOOLCHAIN_DIR:-}" ]; then
    [ -x "$STORYHOOK_RELEASE_TOOLCHAIN_DIR/bin/cargo" ] \
      && [ -x "$STORYHOOK_RELEASE_TOOLCHAIN_DIR/bin/rustc" ] \
      || release_toolchain_die "STORYHOOK_RELEASE_TOOLCHAIN_DIR lacks executable cargo/rustc" \
      || return
    resolved="$STORYHOOK_RELEASE_TOOLCHAIN_DIR"
  else
    local lock_hash toolchain_root marker target staging download_dir
    lock_hash="$(shasum -a 256 "$lock_file" | awk '{print $1}')"
    toolchain_root="$(release_toolchain_root_path "$cache_root" "$lock_hash" "$host_target")"
    marker="$toolchain_root/.storyhook-release-toolchain"
    if [ -x "$toolchain_root/bin/cargo" ] \
      && [ -x "$toolchain_root/bin/rustc" ] \
      && [ "$(cat "$marker" 2>/dev/null)" = "$lock_hash $host_target ${required_targets[*]}" ]; then
      resolved="$toolchain_root"
    else
      download_dir="$cache_root/downloads"
      staging="$cache_root/toolchains/.partial-$host_target-$$"
      mkdir -p "$download_dir" "$staging"
      release_toolchain_install_component rustc "$host_target" "$staging" "$download_dir" "$lock_file" || return
      release_toolchain_install_component cargo "$host_target" "$staging" "$download_dir" "$lock_file" || return
      for target in "${required_targets[@]}"; do
        release_toolchain_install_component rust-std "$target" "$staging" "$download_dir" "$lock_file" || return
      done
      "$staging/bin/rustc" --version >/dev/null \
        || release_toolchain_die "assembled release rustc for $host_target cannot run" \
        || return
      "$staging/bin/cargo" --version >/dev/null \
        || release_toolchain_die "assembled release cargo for $host_target cannot run" \
        || return
      printf '%s\n' "$lock_hash $host_target ${required_targets[*]}" > "$staging/.storyhook-release-toolchain"
      mkdir -p "$(dirname "$toolchain_root")"
      if [ -e "$toolchain_root" ]; then
        release_toolchain_die "incomplete release toolchain cache already exists at $toolchain_root; remove that cache entry and retry"
        return
      fi
      mv "$staging" "$toolchain_root"
      resolved="$toolchain_root"
    fi
  fi

  release_toolchain_verify_sysroot "$resolved" || return
  printf '%s\n' "$resolved"
}
