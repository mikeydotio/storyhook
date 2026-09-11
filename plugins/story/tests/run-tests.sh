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

# --- data-home isolation: the runner's own, never the tests' ----------------
#
# Every test mints its OWN root, daemon and parent pid through lib.sh, exactly
# as `bash test-foo.sh` does on its own -- one code path, not two. This runner
# used to isolate once and export one STORYHOOK_TEST_HOME for the whole run,
# which lib.sh reads as "already isolated": 74 tests then shared one store, one
# daemon, and STORYHOOK_PARENT_PID = this runner, so a daemon that wedged in
# test K failed every test after it (33 of 74 on the night SH-631 was filed)
# and outlived the run by construction. So the variable lib.sh keys on is
# deliberately NOT exported here.
#
# The runner still isolates ITSELF, at a root of its own that nothing writes
# into: this process runs no `story`, but a harness that exports the data dir
# is what `tests/store_isolation.rs` derives the containment set from, and a
# runner that could reach a real store by accident is one refusal short.
# `$STORYHOOK_REAL_HOME` is taken before HOME is rewritten so
# `test-data-home-isolation.sh` can still name the real data home it asserts
# nothing was written to; lib.sh preserves an inherited value.
#
# THE ISOLATION, in one shared place -- `scripts/test-env.sh`, whose own header
# carries the parameters and the reason for each. `--home` IS passed: this
# suite runs nothing but `story` and `git`, so a fake $HOME costs nothing here
# and buys the strongest isolation available -- the harnesses that wrap cargo
# or npm cannot do the same without costing those tools their caches.
export STORYHOOK_REAL_HOME="$HOME"
runner_root="$(mktemp -d /tmp/storyhook-plugin-run.XXXXXX)"

# shellcheck source=../../../scripts/test-env.sh
. "$TESTS_DIR/../../../scripts/test-env.sh"
storyhook_isolate --home "$runner_root"

# Nothing ever starts a daemon under this root, so deleting it is not deleting
# a store from under one -- the case each test's own teardown handles for its
# own root (lib.sh's `_cleanup`).
trap 'rm -rf "$runner_root"' EXIT

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
