#!/usr/bin/env bash
# SH-888: both fixture entry points must select their own tmux server. The
# unrelated caller below is itself test-owned; no host socket is contacted.
ambient=$(mktemp -d /tmp/story-test-host-tmux.XXXXXX)
: > "$ambient/tmux.sock"
printf 'unrelated caller must not be queried\n' > "$ambient/resource_fail"
export TMUX="$ambient/tmux.sock,123,0" TMUX_PANE=%99
source "$(dirname "$0")/lib.sh"
_TMP_REPOS+=("$ambient")

assert_eq "${TMUX:-}" "" "fresh plugin fixture drops inherited caller socket"
assert_eq "${TMUX_PANE:-}" "" "fresh plugin fixture drops inherited caller pane"
repo=$(mk_story_repo)
id=$(new_story "$repo" "Plugin caller isolation")
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id")
assert_ok "$out" true "plugin dispatch ignores unrelated caller server"

# Render the actual wrapper heredoc from the browser runner, not a parallel
# imitation. Its FAKE_* snapshot is restored inside the cleared child, exactly
# where the production allowlist requires the harness bridge to live.
root=$(mktemp -d /tmp/story-test-e2e-dispatch.XXXXXX)
_TMP_REPOS+=("$root")
mkdir -p "$root/faketmux" "$root/knobs"
printf '%s' "$root/faketmux" > "$root/knobs/FAKE_TMUX_STATE"
printf marker > "$root/knobs/FAKE_TMUX_CAPTURE"
printf '%s' "$FAKE_TMUX_PANE_LIFETIME" > "$root/knobs/FAKE_TMUX_PANE_LIFETIME"
python3 - "$TESTS_DIR/../../../scripts/run-e2e.sh" "$root/generate.sh" <<'PY'
import pathlib, sys
source = pathlib.Path(sys.argv[1]).read_text()
start = 'cat >"$STORYHOOK_DISPATCH_SCRIPT" <<WRAPPER'
wrapper = source.split(start, 1)[1].split('\nWRAPPER', 1)[0]
pathlib.Path(sys.argv[2]).write_text(start + wrapper + '\nWRAPPER\n')
PY
render_wrapper() {
  faketmux_env="$root/knobs" STORYHOOK_DISPATCH_SCRIPT="$root/wrapper.sh" \
    _dispatch_protocol="$(sed -n '/^DISPATCH_PROTOCOL=/p' "$SCRIPT")" \
    _real_dispatch_script="$1" command bash "$root/generate.sh"
}
cat > "$root/observe.sh" <<'PROBE'
python3 "$1" select
PROBE
render_wrapper "$root/observe.sh"
out=$(env -u TMUX -u TMUX_PANE -u TMUX_TMPDIR -u FAKE_TMUX_STATE \
  bash "$root/wrapper.sh" "$PLUGIN_ROOT/lib/tmux-target.py")
socket="$(cd "$root/faketmux" && pwd -P)/tmux.sock"
assert_eq "$(jqf "$out" .socket)" "$socket" \
  "browser wrapper selects the fixture even when no host server exists"
render_wrapper "$SCRIPT"

id=$(new_story "$repo" "Browser child isolation")
out=$(cd "$repo" && env -u TMUX -u TMUX_PANE -u TMUX_TMPDIR \
  -u FAKE_TMUX_STATE -u FAKE_TMUX_CAPTURE \
  STORY_TARGET_SESSION=story-fixture-isolation \
  STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 STORY_CONFIRM_DELAY=0 \
  STORY_PASTE_SETTLE_DELAY=0 bash "$root/wrapper.sh" dispatch "$id")
assert_ok "$out" true "browser wrapper dispatch succeeds with cleared tmux selectors"
assert_eq "$(jqf "$out" .prompt_confirmed)" true "browser wrapper confirms its fake provider"
private_git=$(git -C "$repo/.claude/worktrees/$id" rev-parse --absolute-git-dir)
lease="$private_git/storyhook-cleanup-lease-v1.json"
if [ -f "$lease" ]; then
  assert_eq "$(jq -r .tmux.socket_path "$lease")" "$socket" \
    "browser dispatch lease binds only the run-owned socket"
else
  fail_test "browser dispatch did not publish a lease: $out"
fi
[ -s "$root/faketmux/new_window_args.log" ] \
  || fail_test "browser dispatch did not reach this run's fake state"
assert_eq "$(cat "$ambient/resource_fail")" "unrelated caller must not be queried" \
  "unrelated caller fixture remains unchanged"
# End only the placeholder process written by this run's fake.
FAKE_TMUX_STATE="$root/faketmux" "$TESTS_DIR/fakes/tmux" kill-window -t @1
finish
