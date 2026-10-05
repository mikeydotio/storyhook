#!/usr/bin/env bash
# SH-879: a straggler of a finished test starts no daemon and recreates nothing.
#
# The incident: a helper or hook still running when its test ended ran
# `story`. That started a fresh daemon for the test's store, recreated the
# home the test had just deleted, and listened on loopback and on the tailnet
# with no owner left to stop it. This replays it exactly: a nested test owns a
# home and a daemon and exits, and a process carrying that test's environment
# then runs `story` the two ways a straggler does.
source "$(dirname "$0")/lib.sh"

work=$(mktemp -d /tmp/story-test.XXXXXX)
_register_tmp "$work"

# A `tailscale` that records each call. No daemon a test starts may ask it.
mkdir -p "$work/bin"
cat >"$work/bin/tailscale" <<SHIM
#!/bin/sh
printf 'x' >>"$work/probes"
exit 1
SHIM
chmod +x "$work/bin/tailscale"
export PATH="$work/bin:$PATH"

# The finished test: its own lib.sh instance (its own home, lease, daemon and
# owner pid), which records its environment and exits. Its EXIT trap stops
# its daemon and deletes its home, as every test's does.
# shellcheck disable=SC2016 # the inner shell expands its own arguments
env -u STORYHOOK_TEST_HOME -u STORYHOOK_REAL_HOME \
  bash -c 'source "$1/lib.sh" && story daemon start >/dev/null 2>&1 && /usr/bin/env -0 >"$2"' \
  _ "$TESTS_DIR" "$work/finished.env"
assert_eq "$?" "0" "the finished test started its daemon and recorded its environment"

replay=()
child_home=""
while IFS= read -r -d '' entry; do
  replay+=("$entry")
  case "$entry" in STORYHOOK_TEST_HOME=*) child_home="${entry#STORYHOOK_TEST_HOME=}" ;; esac
done <"$work/finished.env"

case "$child_home" in
  /tmp/storyhook-plugin-home.*) _TMP_REPOS+=("$child_home") ;;
  *) fail_test "the finished test recorded its home; got [$child_home]"; finish ;;
esac
[ ! -e "$child_home" ] || fail_test "the finished test deleted its home"

# Whatever a broken straggler starts is this test's to stop, by its exact
# store path, so a red run leaks nothing either. The test's own exit status
# passes through untouched: lib.sh's _cleanup reads it from $?.
stop_strays() {
  local status=$?
  pkill -f -- "--store-path $child_home/" 2>/dev/null
  return "$status"
}
trap 'stop_strays; _cleanup' EXIT

# The finished test's lease is gone with it; a straggler reaches this build
# through whatever else is on PATH, here this test's own lease.
story_bin=$(command -v story)

# 1. A straggler asks for a daemon outright.
if out=$(cd /tmp && env -i "${replay[@]}" "$story_bin" daemon start 2>&1); then
  fail_test "straggler daemon start: a finished test's straggler started a daemon: $out"
fi
assert_contains "$out" "STORYHOOK_PARENT_PID" "straggler daemon start: names the owner contract"

# 2. A straggler runs the SessionStart hook, as a late provider pane does.
printf '%s' '{"hook_event_name":"SessionStart","source":"startup","session_id":"sh879","cwd":"/tmp"}' \
  | (cd /tmp && env -i "${replay[@]}" bash "$PLUGIN_ROOT/hooks/session-start.sh" >/dev/null 2>&1)

[ ! -e "$child_home" ] || fail_test "a straggler recreated the finished test's home [$child_home]"
if pgrep -f -- "--store-path $child_home/" >/dev/null 2>&1; then
  fail_test "a daemon serves the finished test's store: $(pgrep -fl -- "--store-path $child_home/")"
fi
[ ! -e "$work/probes" ] || fail_test "a test daemon asked tailscale for a tailnet address"

finish
