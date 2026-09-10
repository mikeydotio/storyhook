#!/usr/bin/env bash
#
# `storyhook_lease_binary` — pin a shell harness to the Cargo artifact it
# resolved, so a later `cargo build|test|check` in the same checkout cannot
# swap the binary out from under a live run.
#
# THE SHELL RENDERING OF `storyhook_test_support::story_binary()` (SH-532).
# Cargo replaces `target/debug/story` by writing a NEW inode and renaming it
# over the directory entry, so a hard link taken before the rebuild keeps the
# old inode alive, unchanged, for as long as the link exists. The Rust suite has
# done this since SH-532; `scripts/run-e2e.sh` never got the twin, and one
# `cargo test --test council_citations` beside a live browser run voided 167
# tests in 25 minutes (SH-635): the daemon's portfile identity is
# `(version, exe, exe_mtime)`, so the first CLI call after the rebuild read the
# running daemon as somebody else's, stood it down and re-spawned it on a new
# port, and every later test refused the connection.
#
# THE LEASE LIVES BESIDE THE ARTIFACT, NEVER UNDER /private/tmp. A hard link
# cannot cross filesystems (EXDEV), and on this machine the checkout and
# `/private/tmp` are different volumes. The root is therefore
# `<artifact dir>/.storyhook-test-binaries/` — THE SAME directory, and the same
# `<owner pid>-<nonce>` entry shape, as the Rust lease
# (`storyhook_test_support::BINARY_SNAPSHOT_DIR`), so either sweeper reclaims
# the other's dead leases under one policy. `tests/binary_lease.rs` pins the
# name equality and drives these functions for real.
#
# REAPING PROVES ABSENCE, NEVER INFERS IT. A lease is removed only when its
# owner pid is provably gone (`kill -0` fails with ESRCH — "No such process").
# EPERM, a malformed entry name, and every other answer retain the lease: late
# cleanup costs disk; early cleanup takes a live daemon's executable away.
#
# NO COPY FALLBACK. A byte copy taken while Cargo is mid-write can be
# internally inconsistent, and it duplicates a 57 MB payload the link shares
# for free. An unsupported hard link refuses by name with both paths.
#
# HOW TO USE IT:
#
#   . "$(dirname "$0")/binary-lease.sh"
#   story_bin="$(storyhook_lease_binary "$repo_root/target/debug/story")" || exit 1
#   trap 'rm -rf "$(dirname "$story_bin")"' EXIT     # optional: eager cleanup
#
# The lease's owner is `$$` unless a second argument names another pid. The
# parameter exists for the reason the Rust `snapshot_binary_in` takes
# `owner_pid`: a test has to mint a lease it does not own to prove the sweep.

# The one name both renderings agree on. `tests/binary_lease.rs` fails the
# build if this and `storyhook_test_support::BINARY_SNAPSHOT_DIR` diverge.
STORYHOOK_BINARY_LEASE_DIR=".storyhook-test-binaries"

# Whether `pid` is provably absent: `kill -0` failing with ESRCH and nothing
# else. `kill -0` succeeding, or failing with EPERM (a process that exists but
# belongs to somebody else), both answer "might be alive", and this returns 1.
storyhook_binary_lease_owner_is_gone() {
  local pid="$1" err
  case "$pid" in
    '' | *[!0-9]* | 0) return 1 ;;
  esac
  if err="$(kill -0 "$pid" 2>&1)"; then
    return 1
  fi
  case "$err" in
    *"No such process"*) return 0 ;;
    *) return 1 ;;
  esac
}

# Removes every `<pid>-<nonce>` lease under `root` whose owner is provably
# gone. Entries with no parsable pid, and entries whose owner may be alive, stay.
storyhook_sweep_binary_leases() {
  local root="$1" entry name pid
  [ -d "$root" ] || return 0
  for entry in "$root"/*; do
    [ -d "$entry" ] || continue
    name="${entry##*/}"
    case "$name" in
      *-*) pid="${name%%-*}" ;;
      *) continue ;;
    esac
    if storyhook_binary_lease_owner_is_gone "$pid"; then
      rm -rf "$entry"
    fi
  done
}

# Prints the leased path of `artifact` — `<artifact dir>/.storyhook-test-binaries/
# <owner pid>-<nonce>/<basename>` — or refuses by name on stderr and returns 1.
# The basename is preserved so a PATH-based hook resolving `story` finds the
# same executable the harness invokes directly.
storyhook_lease_binary() {
  local artifact="$1" owner_pid="${2:-$$}" dir root lease
  if [ ! -f "$artifact" ]; then
    echo "binary-lease: $artifact is not a file; build it first" >&2
    return 1
  fi
  case "$owner_pid" in
    '' | *[!0-9]* | 0)
      echo "binary-lease: a lease needs an owner pid, got '$owner_pid'" >&2
      return 1
      ;;
  esac
  dir="$(cd "$(dirname "$artifact")" && pwd)"
  root="$dir/$STORYHOOK_BINARY_LEASE_DIR"
  if ! mkdir -p "$root"; then
    echo "binary-lease: cannot create the lease root $root" >&2
    return 1
  fi
  storyhook_sweep_binary_leases "$root"
  if ! lease="$(mktemp -d "$root/${owner_pid}-XXXXXX")"; then
    echo "binary-lease: cannot create a lease directory under $root" >&2
    return 1
  fi
  if ! ln "$artifact" "$lease/$(basename "$artifact")" 2>/dev/null; then
    rm -rf "$lease"
    echo "binary-lease: hard-linking $artifact into $lease failed. Refusing a copy" >&2
    echo "  fallback: a copy taken while Cargo is replacing the artifact can be" >&2
    echo "  internally inconsistent (SH-532). The lease root must be on the same" >&2
    echo "  filesystem as the artifact, which is why it lives beside it." >&2
    return 1
  fi
  printf '%s\n' "$lease/$(basename "$artifact")"
}
