#!/usr/bin/env bash
# SH-672: manual dispatch never consults a lane budget. Exercise the real
# helper, claims and worktrees; only the terminal/provider boundary is fake.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Operator chooses concurrency")

seed_live() {
  : >"$FAKE_TMUX_STATE/agent_windows"
  local i=0
  while [ "$i" -lt "$1" ]; do
    printf 'storyhook:SH-9%02d\tclaude\t0\n' "$i" >>"$FAKE_TMUX_STATE/agent_windows"
    i=$((i + 1))
  done
}

dry() {
  (cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$@" 2>"$FAKE_TMUX_STATE/stderr")
}

for count in 0 4 6 12; do
  seed_live "$count"
  out=$(dry "$id")
  assert_eq "$(jqf "$out" .ok)" true "$count sessions: named dispatch proceeds"
  out=$(dry --next)
  assert_eq "$(jqf "$out" .ok)" true "$count sessions: next dispatch proceeds"
  out=$(dry "$id" --resume)
  assert_eq "$(jqf "$out" .ok)" true "$count sessions: fresh resume proceeds"
  out=$(dry "$id" --auto --full-auto)
  assert_eq "$(jqf "$out" .ok)" true "$count sessions: engine dispatch proceeds"
done

seed_live 6
out=$(dry "$id" --over-budget)
assert_eq "$(jqf "$out" .ok)" true "legacy flag remains accepted"
assert_contains "$(cat "$FAKE_TMUX_STATE/stderr")" "deprecated and has no effect" "legacy flag explains retirement"
out=$(dry "$id" --over-budget --over-budget)
assert_eq "$(jqf "$out" .ok)" false "duplicate legacy flag is still an argument error"

epic=$(cd "$repo" && story new "Epic" --type epic --json | jq -r '.story.story.id')
out=$(dry "$epic" --auto --over-budget)
assert_eq "$(jqf "$out" .ok)" true "legacy no-op also accepts epic starts"

# A tripwire proves the census was not even consulted, including when an
# older binary does not implement the verb. Real CLI behavior is delegated.
STORY_REAL_BIN="$(command -v story)"
export STORY_REAL_BIN
export STORY_CENSUS_CALL_LOG="$FAKE_TMUX_STATE/census-calls"
export STORY_BIN="$TESTS_DIR/fakes/story-no-lane-budget/story"
out=$(dry "$id")
assert_eq "$(jqf "$out" .ok)" true "missing census command does not matter"
[ ! -e "$STORY_CENSUS_CALL_LOG" ] || fail_test "manual dispatch consulted the census"
[ ! -s "$FAKE_TMUX_STATE/stderr" ] || fail_test "ordinary dispatch emitted a census warning"

# The real named and next paths must claim, create a worktree, and deliver
# the charter even with six existing live sessions.
for mode in named next; do
  FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
  export FAKE_TMUX_STATE
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  seed_live 6
  if [ "$mode" = named ]; then target="$id"; else target=--next; fi
  out=$(
    cd "$repo" && TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker bash "$SCRIPT" dispatch "$target" 2>"$FAKE_TMUX_STATE/stderr"
  )
  assert_eq "$(jqf "$out" .ok)" true "$mode: real dispatch succeeds at six"
  assert_eq "$(jqf "$out" .prompt_confirmed)" true "$mode: prompt delivered"
  worktree=$(jqf "$out" .worktree_path)
  [ -d "$worktree" ] || fail_test "$mode: worktree missing"
  [ ! -e "$STORY_CENSUS_CALL_LOG" ] || fail_test "$mode: real dispatch consulted census"
  new_story "$repo" "Next operator-selected task" >/dev/null
done

# Exercise CLI -> daemon -> EngineService -> ShellDispatcher -> real helper.
# Bridge terminal fixture state across the production child's allowlist.
unset STORY_BIN STORY_REAL_BIN STORY_CENSUS_CALL_LOG
story daemon stop --force >/dev/null || fail_test "engine fixture: stop owned daemon"
engine_root="$(mktemp -d /tmp/story-test-engine-capacity.XXXXXX)"
_TMP_REPOS+=("$engine_root")
mkdir -p "$engine_root/bin" "$engine_root/terminals/default"
export STORY_TEST_TERMINALS="$engine_root/terminals"
export STORY_TEST_HELPER="$SCRIPT"
export STORY_TEST_FAKE_TMUX="$TESTS_DIR/fakes/tmux"
export STORYHOOK_DISPATCH_SCRIPT="$engine_root/dispatch.sh"
# Keep synthetic provider processes alive through the bounded fill, then
# terminate their exact fixture-owned windows even when an assertion fails.
dispatch_timeout=$(sed -n 's/^pub const DISPATCH_TIMEOUT: Duration = Duration::from_secs(\([0-9]*\));/\1/p' "$TESTS_DIR/../../../src/service/engine.rs")
[ -n "$dispatch_timeout" ] || fail_test "engine fixture: cannot derive dispatch deadline"
export STORY_TEST_PANE_LIFETIME=$((6 * dispatch_timeout))
cleanup_engine() {
  local status=$? terminal
  story daemon stop --force >/dev/null || status=1
  for terminal in "$STORY_TEST_TERMINALS"/ENG-*; do
    [ -f "$terminal/pane_pid" ] || continue
    FAKE_TMUX_STATE="$terminal" bash "$STORY_TEST_FAKE_TMUX" kill-window -t "%${terminal##*-}" \
      || status=1
  done
  return "$status"
}
trap 'cleanup_engine; _cleanup' EXIT
sed -n '/^DISPATCH_PROTOCOL=/p' "$SCRIPT" >"$STORYHOOK_DISPATCH_SCRIPT"
cat >>"$STORYHOOK_DISPATCH_SCRIPT" <<'DISPATCH'
set -euo pipefail
# ShellDispatcher supplies --project <slug> dispatch <id>.
export STORY_TEST_TERMINAL="$STORY_TEST_TERMINALS/$4"
mkdir -p "$STORY_TEST_TERMINAL"
export FAKE_TMUX_CAPTURE=marker
export STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0
export STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0
exec bash "$STORY_TEST_HELPER" "$@"
DISPATCH
cat >"$engine_root/bin/tmux" <<'TERMINAL'
#!/usr/bin/env bash
set -euo pipefail
export FAKE_TMUX_STATE="${STORY_TEST_TERMINAL:-$STORY_TEST_TERMINALS/default}"
# Each synthetic pane owns the same real placeholder pid and terminal state
# the dispatch helper observed. Route the daemon's exact-pane probes there.
if [ -z "${STORY_TEST_TERMINAL:-}" ]; then
  args=("$@")
  for ((i=0; i<${#args[@]}; i++)); do
    if [ "${args[$i]}" = -t ]; then
      target="${args[$((i + 1))]}"
      case "$target" in
        %*) export FAKE_TMUX_STATE="$STORY_TEST_TERMINALS/ENG-${target#%}" ;;
      esac
    fi
  done
fi
case "$FAKE_TMUX_STATE" in
  */ENG-*) export FAKE_TMUX_PANE_ID="%${FAKE_TMUX_STATE##*-}" ;;
esac
export FAKE_TMUX_CAPTURE=marker
export FAKE_TMUX_PANE_LIFETIME="$STORY_TEST_PANE_LIFETIME"
exec bash "$STORY_TEST_FAKE_TMUX" "$@"
TERMINAL
cat >"$engine_root/bin/claude" <<'PROVIDER'
#!/usr/bin/env bash
exit 0
PROVIDER
chmod +x "$engine_root/bin/tmux" "$engine_root/bin/claude"
export PATH="$engine_root/bin:$PATH"
engine_repo=$(mk_story_repo ENG)
for n in 1 2 3 4 5 6 7; do
  new_story "$engine_repo" "Engine candidate $n" >/dev/null
done
started=$(cd "$engine_repo" && story engine start --lanes 6 --agent claude --json)
assert_eq "$(jqf "$started" .run.lane_count)" 6 "CLI: six lanes configured"
engine_run=$(jqf "$started" .run.id)

# Bound the asynchronous fill by the production timeout for each dispatch.
deadline=$((SECONDS + 6 * dispatch_timeout))
while :; do
  state=$(cd "$engine_repo" && story engine status --run "$engine_run" --json)
  working=$(jqf "$state" '[.run.lanes[] | select(.state == "working")] | length')
  [ "$working" -lt 6 ] || break
  [ "$SECONDS" -lt "$deadline" ] || break
  [ "$(jqf "$state" .run.state)" = running ] || break
  sleep 0.1
done
(cd "$engine_repo" && story engine pause --run "$engine_run" >/dev/null)
[ "$working" = 6 ] || printf '%s\n' "$state" >&2
assert_eq "$working" 6 "CLI: six actual sessions filled"
submitted=0
for receipt in "$STORY_TEST_TERMINALS"/ENG-*/submitted; do
  [ ! -s "$receipt" ] || submitted=$((submitted + 1))
done
assert_eq "$submitted" 6 "CLI: six charters reached provider panes"
story daemon stop --force >/dev/null || fail_test "engine fixture: stop owned daemon"

finish
