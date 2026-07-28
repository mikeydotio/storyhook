#!/usr/bin/env bash
# story.sh test runner. Discovers and runs all test-*.sh files in this
# directory, reports pass/fail. Optional filter: `bash run-tests.sh happy`
# runs only tests whose filename contains 'happy'. Plain bash (no bats) --
# mirrors agentics' plugins/issue/tests/run-tests.sh.
set -uo pipefail
TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FILTER="${1:-}"

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

for test in "$TESTS_DIR"/test-*.sh; do
  name=$(basename "$test")
  if [[ -n "$FILTER" && "$name" != *"$FILTER"* ]]; then
    continue
  fi
  printf '  %-40s ' "$name"
  if bash "$test" >"$LOG" 2>&1; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
  else
    printf 'FAIL\n'
    sed 's/^/      /' "$LOG"
    FAILED+=("$name")
    FAIL=$((FAIL + 1))
  fi
done

rm -f "$LOG"
echo
echo "passed: $PASS  failed: $FAIL"
if [ "$FAIL" -gt 0 ]; then
  echo "failed tests:"
  for f in "${FAILED[@]}"; do echo "  - $f"; done
  exit 1
fi
