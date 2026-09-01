#!/usr/bin/env bash
# story.sh test runner. Discovers and runs all test-*.sh files in this
# directory, reports pass/fail. Optional filter: `bash run-tests.sh happy`
# runs only tests whose filename contains 'happy'. Plain bash (no bats) --
# mirrors agentics' plugins/issue/tests/run-tests.sh.
set -uo pipefail
TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FILTER="${1:-}"

# shellcheck source=../../../scripts/gate-progress.sh
. "$TESTS_DIR/../../../scripts/gate-progress.sh"

# --- data-home isolation, verified before a single test runs ---------------
#
# One workdir for the whole run, so every test sees the same isolated data
# home and a leaked fixture has one place to be. lib.sh mints its own if this
# is unset, which is what keeps `bash test-foo.sh` safe on its own; the point
# of doing it here as well is the assertion below.
export STORYHOOK_REAL_HOME="$HOME"
STORYHOOK_TEST_HOME="$(mktemp -d /tmp/storyhook-plugin-run.XXXXXX)"
export STORYHOOK_TEST_HOME
export HOME="$STORYHOOK_TEST_HOME/home"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
export STORYHOOK_DATA_DIR="$HOME/.local/share/storyhook"
# Outranks STORYHOOK_DATA_DIR (SH-113): an exported one in the developer's shell
# would point this whole suite at their own store.
unset STORYHOOK_STORE_PATH

# Nothing of the developer's own reaches a fixture: not a credential, not a
# project selection somebody else made, and not an override that would disarm
# a guard this run may be testing. There is no harmless value for any of
# these, so they are removed rather than redirected. `story help
# test-environment` names each one and what it protects.
unset STORYHOOK_GITHUB_TOKEN STORYHOOK_PROJECT STORYHOOK_ACTOR
unset STORYHOOK_ALLOW_TEMP_PROJECT STORYHOOK_ALLOW_PROJECT_BURST
unset STORYHOOK_ALLOW_UNINSTALLED_MIGRATION

# Every `story` this suite runs starts a daemon, because since SH-114 every
# `story` does. These two are what stop those daemons poisoning anything: one
# was found alive after a gate run, from this worktree's binary, asking for the
# port a developer's own dashboard uses. A daemon started here can never take
# 3456, and cannot outlive this run.
#
# Duplicated from lib.sh on purpose, and for the same reason the XDG exports
# above are: lib.sh's block is skipped when STORYHOOK_TEST_HOME is already set,
# which is exactly what this script does. Setting them in one place only meant
# the whole-suite run had no isolation at all -- which is how the leaked daemons
# were found.
export STORYHOOK_DAEMON_ADDR="${STORYHOOK_DAEMON_ADDR:-127.0.0.1:0}"
export STORYHOOK_PARENT_PID="$$"
mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$STORYHOOK_DATA_DIR"
trap 'rm -rf "$STORYHOOK_TEST_HOME"' EXIT

# Refuse to run at all rather than run against the developer's real data.
# A suite that writes there fails in the worst possible way: silently, into
# the tracker this project uses to track itself. `/tmp` (never `$TMPDIR`) is
# required because macOS Spotlight indexes `$TMPDIR` and stalls file-heavy
# fixture work (SH-53).
for _var in HOME XDG_DATA_HOME XDG_CONFIG_HOME XDG_STATE_HOME STORYHOOK_DATA_DIR; do
  _value="${!_var:-}"
  if [ -z "$_value" ]; then
    echo "refusing to run: \$$_var is unset -- the suite would write to the real data home" >&2
    exit 1
  fi
  case "$_value" in
  /tmp/* | /private/tmp/*) : ;;
  *)
    echo "refusing to run: \$$_var is [$_value], which is not under /tmp" >&2
    exit 1
    ;;
  esac
done
unset _var _value

PASS=0
FAIL=0
FAILED=()
LOG="$(mktemp /tmp/story-plugin-test-run.XXXXXX)"

known_total=0
for test in "$TESTS_DIR"/test-*.sh; do
  name=$(basename "$test")
  [[ -n "$FILTER" && "$name" != *"$FILTER"* ]] && continue
  known_total=$((known_total + 1))
done
gate_progress_emit_item "release gate/plugin" running "total=$known_total"

for test in "$TESTS_DIR"/test-*.sh; do
  name=$(basename "$test")
  if [[ -n "$FILTER" && "$name" != *"$FILTER"* ]]; then
    continue
  fi
  printf '  %-40s ' "$name"
  # `env -u` keeps this wrapper's own STORYHOOK_GATE_PROGRESS for its own
  # item/case emission above and below, while stripping it from each
  # test-*.sh child -- none shells into gate machinery today, but a harness
  # that isolates the data home neutralizes this the same unconditional way
  # it neutralizes STORYHOOK_STORE_PATH (SH-136 doctrine: defense in depth,
  # not case-by-case reasoning about which child currently needs it).
  if env -u STORYHOOK_GATE_PROGRESS bash "$test" >"$LOG" 2>&1; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
    gate_progress_emit_case "release gate/plugin" pass
  else
    printf 'FAIL\n'
    sed 's/^/      /' "$LOG"
    FAILED+=("$name")
    FAIL=$((FAIL + 1))
    gate_progress_emit_case "release gate/plugin" fail
  fi
done

rm -f "$LOG"
echo
echo "passed: $PASS  failed: $FAIL"
gate_progress_emit_item "release gate/plugin" "$([ "$FAIL" -eq 0 ] && echo passed || echo failed)"
if [ "$FAIL" -gt 0 ]; then
  echo "failed tests:"
  for f in "${FAILED[@]}"; do echo "  - $f"; done
  exit 1
fi
