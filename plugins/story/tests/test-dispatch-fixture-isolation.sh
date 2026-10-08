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
dispatch_owner_tool="$TESTS_DIR/../../../scripts/e2e-dispatch-owners.py"
dispatch_owners="$root/dispatch-owners"
# The real wrapper now registers every invocation before touching fake tmux.
# Keep its actual registry and writer custody, including during fixture cleanup.
# Do not put this root in lib.sh's unconditional temporary-directory removal.
cleanup_browser_fixture() {
  local status=$?
  trap - EXIT
  if ! python3 -B "$dispatch_owner_tool" cleanup "$dispatch_owners" "$root" unused 0 8; then
    # An unsettled helper may still use the repository as well as fake tmux.
    # Preserve the shared fixture roots instead of running generic removal.
    printf 'browser wrapper fixture custody uncertain; retained fixture roots including %s\n' "$root" >&2
    [ "$status" -ne 0 ] || status=1
    exit "$status"
  fi
  (exit "$status")
  _cleanup
}
trap cleanup_browser_fixture EXIT
python3 -B "$dispatch_owner_tool" init "$dispatch_owners" || exit 1
mkdir -p "$root/faketmux" "$root/knobs"
printf '%s' "$root/faketmux" > "$root/knobs/FAKE_TMUX_STATE"
printf marker > "$root/knobs/FAKE_TMUX_CAPTURE"
printf '%s' "$FAKE_TMUX_PANE_LIFETIME" > "$root/knobs/FAKE_TMUX_PANE_LIFETIME"
printf '%s' "$dispatch_owners" > "$root/knobs/FAKE_TMUX_CUSTODY"
printf '%s' "$dispatch_owner_tool" > "$root/knobs/FAKE_TMUX_CUSTODY_HELPER"
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
    dispatch_owner_tool="$dispatch_owner_tool" dispatch_owners="$dispatch_owners" \
    _real_dispatch_script="$1" command bash "$root/generate.sh"
}
run_wrapper() {
  # Match the daemon's process-group boundary without contacting a daemon or
  # adopting the calling test's group as dispatch ownership.
  env -u TMUX -u TMUX_PANE -u TMUX_TMPDIR -u FAKE_TMUX_STATE -u FAKE_TMUX_CAPTURE \
    python3 -c 'import os, sys; os.setpgid(0, 0); os.execvp("bash", ["bash", *sys.argv[1:]])' \
      "$root/wrapper.sh" "$@"
}
cat > "$root/observe.sh" <<'PROBE'
python3 "$1" select
PROBE
render_wrapper "$root/observe.sh"
out=$(run_wrapper "$PLUGIN_ROOT/lib/tmux-target.py")
socket="$(cd "$root/faketmux" && pwd -P)/tmux.sock"
assert_eq "$(jqf "$out" .socket)" "$socket" \
  "browser wrapper selects the fixture even when no host server exists"
render_wrapper "$SCRIPT"

id=$(new_story "$repo" "Browser child isolation")
out=$(cd "$repo" && \
  STORY_TARGET_SESSION=story-fixture-isolation \
  STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 STORY_CONFIRM_DELAY=0 \
  STORY_PASTE_SETTLE_DELAY=0 run_wrapper dispatch "$id")
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
assert_eq "$(jq -s 'map(select(.owner.pid > 0 and (.completed | type) == "array")) | length' \
  "$dispatch_owners"/*.json)" 2 \
  "both browser wrapper invocations retain native completion receipts"
# The EXIT trap settles only identities captured by this fixture's registry
# before removing its fake state, including the completed dispatch placeholder.
finish
