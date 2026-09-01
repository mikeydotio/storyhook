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

# THE ISOLATION, in one shared place -- `scripts/test-env.sh`, whose own header
# carries the parameters and the reason for each. `--home` IS passed: this
# suite runs nothing but `story` and `git`, so a fake $HOME costs nothing here
# and buys the strongest isolation available -- the harnesses that wrap cargo
# or npm cannot do the same without costing those tools their caches.
#
# `$STORYHOOK_REAL_HOME` above survives it so `test-data-home-isolation.sh` can
# still name the real data home it is asserting nothing was written to.
#
# The five-variable refusal loop that used to sit below this is gone: it
# checked each derived path for a `/tmp` prefix, which is now checked once, on
# the root every one of them is derived from.
# shellcheck source=../../../scripts/test-env.sh
. "$TESTS_DIR/../../../scripts/test-env.sh"
storyhook_isolate --home "$STORYHOOK_TEST_HOME"

trap 'rm -rf "$STORYHOOK_TEST_HOME"' EXIT

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
