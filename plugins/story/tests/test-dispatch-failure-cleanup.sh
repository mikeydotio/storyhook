#!/usr/bin/env bash
# SH-675: a failed handoff cannot delete a live agent's cwd or leak lane capacity.
source "$(dirname "$0")/lib.sh"
run_case() {
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-rollback-tmux.XXXXXX)
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  repo=$(mk_story_repo RBK)
  id=$(new_story "$repo" "Refused handoff")
  wt="$repo/.claude/worktrees/$id"
  out=$(cd "$repo" && PATH="$TESTS_DIR/fakes:$PATH" TMUX=fake TMUX_PANE=%0 \
    STORY_READY_DELAY=0 STORY_READY_ATTEMPTS=2 FAKE_TMUX_SUPPRESS_SENTINEL=1 \
    FAKE_TMUX_KILL_WINDOW_PROBE="$wt" bash "$SCRIPT" dispatch "$id")
}
run_case
assert_eq "$(jqf "$out" .ok)" false "readiness refusal"
pid=$(cat "$FAKE_TMUX_STATE/pane_pid" 2>/dev/null || true)
if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
  fail_test "failed dispatch left its owned process alive"
fi
[ ! -e "$wt" ] || fail_test "successful termination did not roll back the new worktree"
assert_eq "$(cat "$FAKE_TMUX_STATE/kill_window_probe.log" 2>/dev/null)" exists \
  "pane termination precedes worktree removal"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r .story.story.state)" todo \
  "verified cleanup releases the claim"
assert_eq "$(PATH="$TESTS_DIR/fakes:$PATH" tmux list-windows -a -F '#{@storyhook-agent}')" "" \
  "a cleaned attempt no longer consumes lane capacity"

FAKE_TMUX_PANE_CHILD=1 run_case
child=$(cat "$FAKE_TMUX_STATE/child_pid")
if kill -0 "$child" 2>/dev/null; then fail_test "startup descendant survived cleanup"; fi
assert_eq "$(jqf "$out" .claimed)" false "terminated process tree releases claim"

FAKE_TMUX_FAIL_KILL_PANE=1 run_case
assert_eq "$(jqf "$out" .claimed)" true "failed stop preserves claim"
[ -d "$wt" ] || fail_test "failed stop deleted worktree"
pid=$(cat "$FAKE_TMUX_STATE/pane_pid")
kill -0 "$pid" 2>/dev/null || fail_test "failed tmux stop killed the diagnostic process"
case "$(ps -o stat= -p "$pid")" in *T*) fail_test "failed stop left process frozen" ;; esac
assert_contains "$(jqf "$out" .display)" "startup cleanup could not be confirmed" "termination error carries context"

FAKE_TMUX_PID_MISMATCH=1 run_case
assert_eq "$(jqf "$out" .claimed)" true "replaced pane preserves claim"
[ -d "$wt" ] || fail_test "replaced pane deleted worktree"
[ ! -f "$FAKE_TMUX_STATE/kill_window_args.log" ] || fail_test "replaced pane was killed"
assert_contains "$(jqf "$out" .display)" "pane identity changed" "ownership error is explicit"

# A failed claim release is still claimed, even after its process and Git
# resources were successfully removed. Intercept only that store operation.
release_bin=$(mktemp -d /tmp/story-test-release-bin.XXXXXX)
_TMP_REPOS+=("$release_bin")
export STORY_REAL_BIN
STORY_REAL_BIN=$(command -v story)
cat > "$release_bin/story" <<'SH'
#!/usr/bin/env bash
for arg in "$@"; do
  if [ "$arg" = unclaim ]; then printf '{"result":"error"}'; exit 1; fi
done
exec "$STORY_REAL_BIN" "$@"
SH
chmod +x "$release_bin/story"
STORY_BIN="$release_bin/story" run_case
assert_eq "$(jqf "$out" .claimed)" true "failed claim release stays claimed"
assert_contains "$(jqf "$out" .display)" "stranded" "failed claim release is actionable"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r .story.story.state)" in-progress \
  "failed claim release matches the store"

# A real terminal exercises process ancestry and tmux's exact-pane semantics.
# A sibling split is not owned by the failed dispatch and must survive.
export PATH="${PATH#"$TESTS_DIR/fakes:"}"
session="story-test-sh675-$$"
_register_tmp_tmux_session "$session"
owned_dir=$(mktemp -d /tmp/story-test-sh675-processes.XXXXXX)
_TMP_REPOS+=("$owned_dir")
cat > "$owned_dir/parent.py" <<'PY'
import pathlib
import subprocess
import sys
import time
child = subprocess.Popen(["sleep", "30"])
pathlib.Path(sys.argv[1]).write_text(str(child.pid))
time.sleep(30)
PY
pane=$(tmux new-session -d -s "$session" -P -F '#{pane_id}' \
  "python3 $owned_dir/parent.py $owned_dir/child")
pid=$(tmux display-message -p -t "$pane" '#{pane_pid}')
sibling=$(tmux split-window -h -d -t "$pane" -P -F '#{pane_id}' 'sleep 30')
for _ in {1..100}; do [ ! -s "$owned_dir/child" ] || break; sleep 0.05; done
if [ ! -s "$owned_dir/child" ]; then
  tmux capture-pane -p -t "$pane" >&2
  printf 'tmux=%s pane=%s pid=%s\n' "$(command -v tmux)" "$pane" "$pid" >&2
  fail_test "real process fixture did not start its child"
  finish
fi
child=$(cat "$owned_dir/child")
stop_result=$(python3 "$PLUGIN_ROOT/lib/stop-dispatch-pane.py" "$pane" "$pid")
assert_eq "$(jqf "$stop_result" .ok)" true "real startup tree terminated"
if kill -0 "$pid" 2>/dev/null || kill -0 "$child" 2>/dev/null; then
  fail_test "real owned parent or descendant remains alive"
fi
tmux display-message -p -t "$sibling" '#{pane_pid}' >/dev/null \
  || fail_test "cleanup killed an unrelated sibling pane"
finish
