#!/usr/bin/env bash
# story.sh test runner. Discovers and runs all test-*.sh files in this
# directory, reports pass/fail. Optional filter: `bash run-tests.sh happy`
# runs only tests whose filename contains 'happy'. Plain bash (no bats) --
# mirrors agentics' plugins/issue/tests/run-tests.sh.
#
# --- the pool (SH-783) -------------------------------------------------------
#
# Up to $STORYHOOK_PLUGIN_JOBS scripts run at once. Every script already mints
# its own root, store, daemon, port and fake tmux through lib.sh (see the
# isolation note below), so running two at once shares nothing but the
# machine. One at a time, the leg took 870-1870 s for 107 scripts.
#
# A script whose assertions depend on timing that concurrent siblings could
# move carries a `# plugin-runner: serial` line, and says why beside it. The
# serial lane runs after the pool has drained, one script at a time, so it
# sees only the load it would have seen before the pool existed.
#
# The report is written in one fixed order -- the pool's scripts in discovery
# order, then the serial lane's -- whatever order they finish in, so two runs
# of one tree print the same report. The gate journal gets each case when its
# script is reaped, from this process only: `gate_progress_emit_case` is a
# plain append, and one writer keeps it whole.
#
# Jobs stay in this process's group (no `set -m`), so a signal to the group
# reaches them. A signal to this process alone is forwarded down every running
# script's process tree before the signal is re-raised, so a cancelled leg
# never leaves a script behind it.
set -uo pipefail
TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FILTER="${1:-}"

JOBS="${STORYHOOK_PLUGIN_JOBS:-1}"
case "$JOBS" in
('' | *[!0-9]* | 0*)
  echo "run-tests.sh: STORYHOOK_PLUGIN_JOBS must be a positive integer, got '$JOBS'" >&2
  exit 2
  ;;
esac

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
LOGS="$(mktemp -d /tmp/story-plugin-test-run.XXXXXX)"
trap 'rm -rf "$runner_root" "$LOGS"' EXIT

pooled=()
serial=()
for test in "$TESTS_DIR"/test-*.sh; do
  name=$(basename "$test")
  [[ -n "$FILTER" && "$name" != *"$FILTER"* ]] && continue
  if grep -q '^# plugin-runner: serial' "$test"; then
    serial+=("$test")
  else
    pooled+=("$test")
  fi
done
# `${a[@]+"${a[@]}"}`: bash 3.2 calls an empty array unbound under `set -u`.
order=(${pooled[@]+"${pooled[@]}"} ${serial[@]+"${serial[@]}"})
total=${#order[@]}
npooled=${#pooled[@]}
gate_progress_emit_item "release gate/plugin" running "total=$total"

PASS=0
FAIL=0
FAILED=()
pids=()
verdict=()
seconds=()
started=()

# Launches order[$1]. The exit status reaches the parent through a file
# renamed into place, so the poll below never reads a half-written one and
# never has to guess from a pid that may already be reused.
#
# `env -u` keeps this wrapper's own STORYHOOK_GATE_PROGRESS for its own item
# and case emission while stripping it from each test-*.sh child -- none shells
# into gate machinery today, but a harness that isolates the data home
# neutralizes this the same unconditional way it neutralizes
# STORYHOOK_STORE_PATH (SH-136 doctrine: defense in depth, not case-by-case
# reasoning about which child currently needs it).
launch() {
  local i="$1"
  started[$i]=$SECONDS
  (
    env -u STORYHOOK_GATE_PROGRESS bash "${order[$i]}" >"$LOGS/$i.log" 2>&1 </dev/null
    echo "$?" >"$LOGS/$i.rc.tmp"
    mv "$LOGS/$i.rc.tmp" "$LOGS/$i.rc"
  ) &
  pids[$i]=$!
}

# Prints $1 and every descendant of it, parents first.
process_tree() {
  local child
  echo "$1"
  for child in $(pgrep -P "$1" 2>/dev/null); do
    process_tree "$child"
  done
}

# Forwards a signal to every running script's whole tree, gives them a grace
# period to run their own teardown, kills what is left, then dies of the same
# signal so the caller sees a cancellation rather than a verdict.
on_signal() {
  local signal="$1" i tree="" pid deadline
  trap - TERM INT HUP
  for ((i = 0; i < ${#order[@]}; i++)); do
    [ -n "${pids[$i]:-}" ] && [ -z "${verdict[$i]:-}" ] || continue
    tree="$tree $(process_tree "${pids[$i]}")"
  done
  for pid in $tree; do kill -TERM "$pid" 2>/dev/null; done
  deadline=$((SECONDS + 5))
  for pid in $tree; do
    while kill -0 "$pid" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do sleep 0.1; done
    kill -KILL "$pid" 2>/dev/null
  done
  rm -rf "$runner_root" "$LOGS"
  trap - EXIT
  kill -s "$signal" "$$"
}
trap 'on_signal TERM' TERM
trap 'on_signal INT' INT
trap 'on_signal HUP' HUP

next=0
flushed=0
running=0
while [ "$flushed" -lt "$total" ]; do
  # The pool admits up to $JOBS; the serial lane admits one script, and only
  # once nothing else is running.
  while [ "$next" -lt "$total" ]; do
    if [ "$next" -lt "$npooled" ]; then
      [ "$running" -lt "$JOBS" ] || break
    else
      [ "$running" -eq 0 ] || break
    fi
    launch "$next"
    running=$((running + 1))
    next=$((next + 1))
  done

  for ((i = flushed; i < next; i++)); do
    [ -z "${verdict[$i]:-}" ] && [ -e "$LOGS/$i.rc" ] || continue
    wait "${pids[$i]}" 2>/dev/null
    seconds[$i]=$((SECONDS - started[$i]))
    running=$((running - 1))
    if [ "$(cat "$LOGS/$i.rc")" = 0 ]; then
      verdict[$i]=PASS
      gate_progress_emit_case "release gate/plugin" pass
    else
      verdict[$i]=FAIL
      gate_progress_emit_case "release gate/plugin" fail
    fi
  done

  while [ "$flushed" -lt "$next" ] && [ -n "${verdict[$flushed]:-}" ]; do
    name=$(basename "${order[$flushed]}")
    printf '  %-40s %s\n' "$name" "${verdict[$flushed]}"
    if [ "${verdict[$flushed]}" = PASS ]; then
      PASS=$((PASS + 1))
    else
      sed 's/^/      /' "$LOGS/$flushed.log"
      FAILED+=("$name")
      FAIL=$((FAIL + 1))
    fi
    flushed=$((flushed + 1))
  done

  [ "$flushed" -lt "$total" ] && sleep 0.1
done

echo
echo "passed: $PASS  failed: $FAIL"
# The evidence for the next person who asks where this leg's time goes.
if [ "$total" -gt 0 ]; then
  printf 'slowest (jobs=%s):' "$JOBS"
  for ((i = 0; i < total; i++)); do
    printf '%s %s\n' "${seconds[$i]}" "$(basename "${order[$i]}")"
  done | sort -rn | head -5 | while read -r secs name; do printf ' %s %ss' "$name" "$secs"; done
  echo
fi
gate_progress_emit_item "release gate/plugin" "$([ "$FAIL" -eq 0 ] && echo passed || echo failed)"
if [ "$FAIL" -gt 0 ]; then
  echo "failed tests:"
  for f in "${FAILED[@]}"; do echo "  - $f"; done
  exit 1
fi
