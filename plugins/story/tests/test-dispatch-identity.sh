#!/usr/bin/env bash
# SH-677: failed registration rolls back before the charter can be delivered.
source "$(dirname "$0")/lib.sh"
repo=$(mk_story_repo)
id=$(new_story "$repo" "Registration failure")
out=$(cd "$repo" && PATH="$TESTS_DIR/fakes:$PATH" TMUX=fake TMUX_PANE=%0 \
  STORY_READY_DELAY=0 STORY_CONFIRM_DELAY=0 FAKE_TMUX_CAPTURE=marker \
  FAKE_TMUX_FAIL_IDENTITY_WRITE=1 bash "$SCRIPT" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" false "registration refuses"
assert_eq "$(jqf "$out" .reason)" pane-identity-unavailable "typed registration refusal"
assert_eq "$(jqf "$out" .claimed)" false "registration failure releases claim"
assert_contains "$(jqf "$out" .display)" "No story charter was delivered" "failure names delivery boundary"
[ ! -e "$repo/.claude/worktrees/$id" ] || fail_test "registration failure leaked worktree"
[ ! -s "$FAKE_TMUX_STATE/pastes.log" ] || fail_test "registration failure pasted the charter"
pid=$(cat "$FAKE_TMUX_STATE/stopped_pid")
[ -n "$pid" ] || fail_test "registration rollback did not stop its pane"
if kill -0 "$pid" 2>/dev/null; then fail_test "registration failure left owned agent alive"; fi
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r .story.story.state)" todo \
  "registration rollback matches store"

# Supply stale or unavailable launch evidence while keeping the real process
# alive. Only the OS observation boundary is fault-injected; registration,
# rollback, Git cleanup, and the story store all run production code.
adapter=$(mktemp -d /tmp/story-test-launch-token.XXXXXX)
_TMP_REPOS+=("$adapter")
export FIXTURE_REAL_PYTHON
FIXTURE_REAL_PYTHON=$(command -v python3)
cat > "$adapter/python3" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" = */agent_identity.py && "${2:-}" = capture ]]; then
  if [ "$FIXTURE_TOKEN_MODE" = missing ]; then
    printf '{"ok":false,"display":"fixture launch probe unavailable"}\n'
    exit 1
  fi
  "$FIXTURE_REAL_PYTHON" "$@" | "$FIXTURE_REAL_PYTHON" -c \
    'import json,sys; value=json.load(sys.stdin); value["identity"]["start"]="earlier-launch"; print(json.dumps(value))'
  exit
fi
exec "$FIXTURE_REAL_PYTHON" "$@"
SH
chmod +x "$adapter/python3"
for mode in stale missing; do
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-token-tmux.XXXXXX)
  export FAKE_TMUX_STATE
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  repo=$(mk_story_repo)
  id=$(new_story "$repo" "Unproved launch ownership: $mode")
  out=$(cd "$repo" && PATH="$adapter:$TESTS_DIR/fakes:$PATH" TMUX=fake TMUX_PANE=%0 \
    STORY_READY_DELAY=0 STORY_CONFIRM_DELAY=0 FAKE_TMUX_CAPTURE=marker \
    FIXTURE_TOKEN_MODE="$mode" bash "$SCRIPT" dispatch "$id")
  assert_eq "$(jqf "$out" .ok)" false "$mode: registration refuses"
  assert_eq "$(jqf "$out" .claimed)" true "$mode: unproved owner retains claim"
  assert_contains "$(jqf "$out" .display)" "launch start" "$mode: diagnostics name missing ownership evidence"
  [ -d "$repo/.claude/worktrees/$id" ] || fail_test "$mode: unproved owner lost its worktree"
  [ ! -f "$FAKE_TMUX_STATE/stopped_pid" ] || fail_test "$mode: rollback terminated the unproved owner"
  pid=$(cat "$FAKE_TMUX_STATE/pane_pid" 2>/dev/null || true)
  kill -0 "$pid" 2>/dev/null || fail_test "$mode: live process did not survive"
  if [ -n "$pid" ]; then
    case "$(ps -o stat= -p "$pid")" in *T*) fail_test "$mode: process left frozen" ;; esac
  fi
  assert_eq "$(cd "$repo" && story show "$id" --json | jq -r .story.story.state)" in-progress \
    "$mode: store preserves the claim"
done
finish
