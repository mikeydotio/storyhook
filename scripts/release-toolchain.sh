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

ensure_release_toolchain() {
  local host_target="$1"
  local cache_root="$2"
  local lock_file="$3"
  shift 3
  local required_targets=("$@")

  if [ -n "${STORYHOOK_RELEASE_TOOLCHAIN_DIR:-}" ]; then
    [ -x "$STORYHOOK_RELEASE_TOOLCHAIN_DIR/bin/cargo" ] \
      && [ -x "$STORYHOOK_RELEASE_TOOLCHAIN_DIR/bin/rustc" ] \
      || release_toolchain_die "STORYHOOK_RELEASE_TOOLCHAIN_DIR lacks executable cargo/rustc" \
      || return
    printf '%s\n' "$STORYHOOK_RELEASE_TOOLCHAIN_DIR"
    return
  fi

  local lock_hash toolchain_root marker target staging download_dir
  lock_hash="$(shasum -a 256 "$lock_file" | awk '{print $1}')"
  toolchain_root="$cache_root/toolchains/$lock_hash/$host_target"
  marker="$toolchain_root/.storyhook-release-toolchain"
  if [ -x "$toolchain_root/bin/cargo" ] \
    && [ -x "$toolchain_root/bin/rustc" ] \
    && [ "$(cat "$marker" 2>/dev/null)" = "$lock_hash $host_target ${required_targets[*]}" ]; then
    printf '%s\n' "$toolchain_root"
    return
  fi

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
  printf '%s\n' "$toolchain_root"
}
