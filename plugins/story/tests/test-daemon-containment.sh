#!/usr/bin/env bash
# SH-631: every test owns its daemon, and teardown stands that daemon down
# before deleting the home it holds.
#
# Two invariants, both measured rather than inferred from the harness's text:
#
#   1. THIS process is the daemon's parent. `run-tests.sh` used to isolate once
#      and export one STORYHOOK_TEST_HOME for the whole run, so every test
#      shared one store, one daemon, and STORYHOOK_PARENT_PID = the runner --
#      which is what let one wedged daemon fail every later test (33 of 74 on
#      the night SH-631 was filed) instead of one. Under the runner, this
#      assertion is what used to be red.
#
#   2. A test's daemon is gone the moment the test is, and its home does not
#      come back. The old teardown deleted the home under a live daemon; the
#      daemon noticed its parent had died up to one SHUTDOWN_CHECK (250ms) later
#      and its exit journal (`Journal::append` -> create_dir_all) resurrected
#      the directory -- 77 such corpses were sitting in /tmp on the filing
#      machine, each holding nothing but "parent process N is gone; exiting".
#      That window is also exactly what the gate postlude reported as "a daemon
#      serving a store that no longer exists".
#
# What is NOT tested here, and why: the path where `story daemon stop --force`
# itself fails at teardown needs a daemon that survives SIGKILL by identity,
# which no fixture can arrange honestly. The harness fails the test loudly on
# that path; the message is asserted nowhere because it cannot be provoked.
source "$(dirname "$0")/lib.sh"

# --- 1. this test is its daemon's parent ------------------------------------
assert_eq "${STORYHOOK_PARENT_PID:-}" "$$" \
  "containment: STORYHOOK_PARENT_PID names this test, not a runner"
case "${STORYHOOK_TEST_HOME:-}" in
  /tmp/storyhook-plugin-home.* | /private/tmp/storyhook-plugin-home.*) : ;;
  *) fail_test "containment: home was inherited rather than minted per test: ${STORYHOOK_TEST_HOME:-unset}" ;;
esac

# --- 2. a child test's daemon dies with the child; its home stays deleted -----
# A second, independent test process: STORYHOOK_TEST_HOME is unset so lib.sh
# mints a fresh root and parents the daemon to the child. The child reports
# where its daemon lives and what its pid is, then exits through lib.sh's own
# EXIT trap -- the code under test.
report=$(mktemp /tmp/story-test-containment.XXXXXX)
_TMP_REPOS+=("$report")
env -u STORYHOOK_TEST_HOME -u STORYHOOK_REAL_HOME bash -c '
  source "$1"
  repo=$(mk_story_repo)
  (cd "$repo" && story list >/dev/null) || exit 97
  portfile=$(ls "$STORYHOOK_TEST_HOME"/home/.local/state/storyhook/daemons/*/daemon.json 2>/dev/null | head -1)
  [ -n "$portfile" ] || exit 98
  printf "%s\n%s\n" "$STORYHOOK_TEST_HOME" "$(jq -r .pid "$portfile")" >"$2"
' _ "$TESTS_DIR/lib.sh" "$report"
child_status=$?
assert_eq "$child_status" "0" "containment: child test process ran and tore down cleanly"

child_home=$(sed -n 1p "$report")
child_daemon=$(sed -n 2p "$report")
[ -n "$child_home" ] && [ -n "$child_daemon" ] \
  || fail_test "containment: child did not report its home and daemon pid"

# Immediately, not after the parent-watch window: teardown stopped the daemon
# itself rather than leaving it to notice the child had gone.
if kill -0 "$child_daemon" 2>/dev/null; then
  fail_test "containment: child's daemon (pid $child_daemon) outlived the child"
fi
[ ! -e "$child_home" ] \
  || fail_test "containment: child's home survived its EXIT trap at $child_home"

# The resurrection window: a daemon exiting on its parent watch writes its
# journal within SHUTDOWN_CHECK (250ms) of the parent's death. Waiting four of
# those is derived from that bound, not chosen; if the home is back after it,
# something outlived the test and wrote into a directory that was deleted.
sleep 1
[ ! -e "$child_home" ] \
  || fail_test "containment: child's home was resurrected after teardown at $child_home ($(find "$child_home" -type f | head -3 | tr '\n' ' '))"

finish
