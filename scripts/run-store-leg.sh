#!/usr/bin/env bash
#
# Runs the integration suite against the store-backed invoker.
#
# The strangler's proof engine: the same tests, the same fixtures, the same
# assertions — served by `STORYHOOK_INVOKER=local` instead of by `.storyhook/`.
# A failure here is a thing the flip would break, found while the legacy path is
# still the default.
#
# Usage: run-store-leg.sh [--exclude-file <substring>]...
#
# `--exclude-file` names one test binary exactly; `--exclude-prefix` removes a
# family (`--exclude-prefix store_` takes every `tests/store_*.rs`);
# `--skip-test` removes one *test*, so a file's other twenty stay in the leg
# instead of being lost to one white-box assertion. Every entry is justified —
# file, reason, burn-down wave — in docs/rearch/flip-checklist.md, section G;
# this script only applies them.
#
# Why a script and not a `cargo test` invocation: cargo has no "run every
# integration test except these" flag, so the target list has to be built from
# the tree and passed as explicit `--test` arguments.

set -euo pipefail

cd "$(dirname "$0")/.."

exact=()
prefixes=()
skips=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --exclude-file)
      exact+=("$2")
      shift 2
      ;;
    --exclude-prefix)
      prefixes+=("$2")
      shift 2
      ;;
    --skip-test)
      # One test, not a file. Every bare test name is unique workspace-wide
      # (scripts/capture-baseline.sh asserts it), so libtest's `--skip` can
      # name one precisely — which keeps a file's other twenty tests in the leg
      # instead of losing them to one white-box assertion.
      skips+=(--skip "$2")
      shift 2
      ;;
    *)
      echo "run-store-leg.sh: unknown argument \`$1\`" >&2
      exit 2
      ;;
  esac
done

# Every integration test target, minus the exclusions.
targets=()
skipped=()
for path in tests/*.rs; do
  name="$(basename "$path" .rs)"
  excluded=false
  # `${a[@]+"${a[@]}"}` rather than `"${a[@]}"`: under `set -u`, bash 3.2 —
  # which is what macOS ships — treats an empty array as unset, so the plain
  # form aborts the script when no exclusions were passed. Running the leg
  # with none is the documented usage and has to work.
  for one in ${exact[@]+"${exact[@]}"}; do
    if [[ "$name" == "$one" ]]; then
      excluded=true
      break
    fi
  done
  if ! $excluded; then
    for prefix in ${prefixes[@]+"${prefixes[@]}"}; do
      if [[ "$name" == "$prefix"* ]]; then
        excluded=true
        break
      fi
    done
  fi
  if $excluded; then
    skipped+=("$name")
  else
    targets+=(--test "$name")
  fi
done

if [[ ${#targets[@]} -eq 0 ]]; then
  echo "run-store-leg.sh: every test target is excluded — that cannot be right" >&2
  exit 1
fi

echo "store leg: $(( ${#targets[@]} / 2 )) target(s), ${#skipped[@]} file(s) excluded, $(( ${#skips[@]} / 2 )) test(s) skipped"
echo "  excluded: ${skipped[*]-(none)}"
echo

# A private data directory for the whole run, and this is not optional.
#
# `storyhook_test_support::TestEnv` isolates the child processes of tests that
# use it — but ~45 files still build their own fixtures and inherit this
# process's environment wholesale, so without this the store leg writes every
# fixture project into the developer's real `~/.local/share/storyhook/store.db`.
# It did, once, before this block existed. `STORYHOOK_DATA_DIR` overrides the
# XDG lookup entirely, and a `TestEnv`-using test overrides it again with its
# own equally-isolated directory.
#
# `/private/tmp`, not `$TMPDIR`: the latter is Spotlight-indexed on macOS and
# this run creates a database plus its write-ahead log per test binary.
leg_root="$(mktemp -d /private/tmp/storyhook-store-leg.XXXXXX)"
trap 'rm -rf "$leg_root"' EXIT
export STORYHOOK_DATA_DIR="$leg_root/data"
export XDG_STATE_HOME="$leg_root/state"

# `local`, not `legacy`: the whole point. `main.rs` exits 2 on an unrecognised
# value, so a typo here fails loudly rather than quietly re-running the legacy
# leg and reporting a meaningless green.
export STORYHOOK_INVOKER=local
export INSTA_UPDATE=no
# `--no-fail-fast`: the leg is a *survey* as much as a gate. Stopping at the
# first red binary hides how much of the suite the flip would break, which is
# the one number the burn-down is planned against.
cargo test --workspace --no-fail-fast "${targets[@]}" -- ${skips[@]+"${skips[@]}"}
