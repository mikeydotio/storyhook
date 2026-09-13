#!/usr/bin/env bash
# Guarded continuation preserves leased work and never kills a live replacement.
source "$(dirname "$0")/lib.sh"

out=$(STORY_DRY_RUN=1 bash "$SCRIPT" dispatch SH-1 --require-absent 2>&1)
assert_contains "$out" '--require-absent requires --resume and --continuation-file' \
  'guard: absent-only mode requires an explicit persisted continuation'
out=$(STORY_DRY_RUN=1 bash "$SCRIPT" dispatch SH-1 --continuation-file=/tmp/missing 2>&1)
assert_contains "$out" '--continuation-file requires --require-absent' \
  'guard: a capture file cannot alter ordinary dispatch'

# Production dispatcher and Git inventory execute; the runtime adapter is the
# external process boundary. Its ownership decisions have separate real-process
# tests. Here a receipt permits inspection of the downstream launch primitive.
fixture=$(mktemp -d /tmp/story-continuation-dispatch.XXXXXX)
_TMP_REPOS+=("$fixture")
mkdir -p "$fixture/bin"
real_python=$(command -v python3)
real_story=$(command -v story)
cat >"$fixture/bin/python3" <<'PY'
#!/usr/bin/env bash
case "${1:-}" in
  */continuation_runtime.py)
    input=$(cat)
    printf '%s\t%s\n' "$2" "$input" >>"$CONTINUATION_CALLS"
    case "$2" in
      resume-preflight) printf '{"ok":true}' ;;
      register) printf '{"ok":true,"capture":{}}' ;;
      *) exit 1 ;;
    esac ;;
  *) exec "$CONTINUATION_REAL_PYTHON" "$@" ;;
esac
PY
chmod +x "$fixture/bin/python3"
export CONTINUATION_REAL_PYTHON="$real_python" CONTINUATION_CALLS="$fixture/calls"
cat >"$fixture/bin/story" <<'STORY'
#!/usr/bin/env bash
case "$*" in
  *'continuation capabilities'*) printf '{"result":"ok","continuation_protocol":1}' ;;
  *) exec "$CONTINUATION_REAL_STORY" "$@" ;;
esac
STORY
chmod +x "$fixture/bin/story"
export CONTINUATION_REAL_STORY="$real_story"
cat >"$fixture/bin/tmux" <<'TMUX'
#!/usr/bin/env bash
if [ "$1" = respawn-pane ] && [ "${CONTINUATION_REFUSE_RESPAWN:-}" = 1 ]; then
  exit 1
fi
exec "$CONTINUATION_FAKE_TMUX" "$@"
TMUX
chmod +x "$fixture/bin/tmux"
export CONTINUATION_FAKE_TMUX="$TESTS_DIR/fakes/tmux"

repo=$(mk_story_repo GCR)
repo=$(cd "$repo" && pwd -P)
id=$(new_story "$repo" 'Guarded continuation on retained dirty work')
mk_dispatched "$repo" "$id" >/dev/null
worktree="$repo/.claude/worktrees/$id"
printf 'preserve this\n' >"$worktree/retained.txt"
(cd "$repo" && story move "$id" in-progress >/dev/null)
record="$fixture/record.json"
socket="$fixture/tmux.sock"
jq -n --arg id "$id" --arg repo "$repo" --arg wt "$worktree" --arg socket "$socket" \
  '{story_id:$id,capture:{provider:"claude",model:"opusplan",effort:"",speed:"standard",
    autonomy_mode:"auto",mode:"default",pane:"%1",socket:$socket,
    lease:{version:1,project_slug:"gcr",story_id:$id,repository_path:$repo,
      worktree_path:$wt,branch:("worktree-"+$id),tmux:{socket_path:$socket}}}}' >"$record"

export FAKE_TMUX_STATE="$fixture/tmux-state"
mkdir -p "$FAKE_TMUX_STATE"
export FAKE_TMUX_PANES="$id"$'\t1\t%1'
export FAKE_TMUX_DEAD=1
export FAKE_TMUX_PANE_COMMAND=claude
mkdir -p "$worktree/.claude"
printf 'old sentinel\n' >"$worktree/.claude/dispatch-sentinel.json"
private_git=$(git -C "$worktree" rev-parse --absolute-git-dir)
printf 'old lease\n' >"$private_git/storyhook-cleanup-lease-v1.json"
override=$(cd "$repo" && PATH="$fixture/bin:$TESTS_DIR/fakes:$PATH" \
  TMUX="$socket,0,0" TMUX_PANE=%0 STORY_AUTO_PROMPT='Custom charter' STORY_DRY_RUN=1 \
  bash "$SCRIPT" dispatch "$id" --auto --resume --require-absent \
    --continuation-file="$record" 2>&1)
assert_eq "$(jqf "$override" .ok)" false 'guarded recovery requires its registered builtin charter'
launch_override=$(cd "$repo" && PATH="$fixture/bin:$TESTS_DIR/fakes:$PATH" \
  TMUX="$socket,0,0" TMUX_PANE=%0 STORY_LAUNCH_CMD='exec claude --model other' STORY_DRY_RUN=1 \
  bash "$SCRIPT" dispatch "$id" --auto --resume --require-absent \
    --continuation-file="$record" 2>&1)
assert_eq "$(jqf "$launch_override" .ok)" false 'guarded recovery refuses launch settings overrides'
no_socket=$(cd "$repo" && PATH="$fixture/bin:$TESTS_DIR/fakes:$PATH" \
  TMUX='' STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" --auto --resume --require-absent \
    --continuation-file="$record" 2>&1)
assert_eq "$(jqf "$no_socket" .ok)" false 'guarded recovery requires captured native socket'
race=$(cd "$repo" && PATH="$fixture/bin:$TESTS_DIR/fakes:$PATH" \
  TMUX="$socket,0,0" TMUX_PANE=%0 CONTINUATION_REFUSE_RESPAWN=1 \
  bash "$SCRIPT" dispatch "$id" --auto --resume --require-absent \
    --continuation-file="$record" 2>&1)
assert_eq "$(jqf "$race" .ok)" false 'tmux refuses a pane that became live after preflight'
assert_eq "$(cat "$worktree/.claude/dispatch-sentinel.json" 2>/dev/null)" 'old sentinel' \
  'atomic refusal preserves the other owner sentinel'
assert_eq "$(cat "$private_git/storyhook-cleanup-lease-v1.json" 2>/dev/null)" 'old lease' \
  'atomic refusal preserves the inherited cleanup lease'
out=$(cd "$repo" && PATH="$fixture/bin:$TESTS_DIR/fakes:$PATH" \
  TMUX="$socket,0,0" TMUX_PANE=%0 STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
  STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
  bash "$SCRIPT" dispatch "$id" --auto --resume --require-absent \
    --continuation-file="$record" 2>&1)
assert_eq "$(jqf "$out" .ok)" true "guarded resume succeeds over retained work: $out"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" \
  'Unknown capacity alone must not defer already assigned work' \
  'a missing token counter cannot defer every fresh assignment'
assert_eq "$(cat "$worktree/retained.txt")" 'preserve this' 'dirty bytes survive guarded resume'
args=$(cat "$FAKE_TMUX_STATE/respawn_pane_args.log")
case "$args" in *'-k'*) fail_test 'guarded respawn must omit -k' ;; esac
assert_contains "$args" "-c $worktree" 'guarded respawn uses captured worktree'
assert_contains "$(cat "$CONTINUATION_CALLS")" 'resume-preflight' 'ownership preflight reaches runtime'

unset FAKE_TMUX_PANES FAKE_TMUX_DEAD
missing=$(cd "$repo" && PATH="$fixture/bin:$TESTS_DIR/fakes:$PATH" \
  TMUX="$socket,0,0" TMUX_PANE=%0 STORY_DRY_RUN=1 \
  bash "$SCRIPT" dispatch "$id" --auto --resume --require-absent \
    --continuation-file="$record" 2>&1)
# A missing pane is not an invitation to allocate another story window.
assert_eq "$(jqf "$missing" .ok)" false 'missing retained pane refuses automatic recreation'
finish
