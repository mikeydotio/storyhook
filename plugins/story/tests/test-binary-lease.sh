#!/usr/bin/env bash
# SH-639: every test runs a LEASE of the binary under test, never Cargo's own
# artifact path -- the plugin-leg twin of SH-635 (the browser runner) and
# SH-532 (the Rust suite).
#
# Cargo replaces `target/debug/story` by writing a new inode and renaming it
# over the directory entry. A daemon's portfile identity is
# `(version, exe, exe_mtime)` (`DaemonInfo::is_this_binary`), so with the bare
# artifact on PATH a `cargo build|test|check` landing mid-test changed the
# identity of the test's own daemon: the next `story` call read it as somebody
# else's, stood it down and restarted it on a fresh port, silently. A hard link
# taken before the rebuild keeps the old inode alive, unchanged, for as long as
# the link exists, so `command -v story` keeps resolving the build the test
# started with and the daemon keeps its identity.
#
# Three invariants, each measured against a REAL daemon rather than inferred
# from lib.sh's text:
#
#   1. `story` resolves to a lease owned by this test (`<pid>-<nonce>` under
#      `.storyhook-test-binaries/` beside the artifact), sharing the artifact's
#      inode at resolution time.
#   2. Replacing the artifact the way Cargo does leaves `story` resolving the
#      leased inode, and the test's daemon keeps its pid -- the observable the
#      defect was filed on.
#   3. A nested lib.sh instance that INHERITS its caller's home reuses the
#      caller's lease rather than minting a second one: identity is a path
#      compare, so a second lease path would itself trigger the restart this
#      story removes (test-temp-cleanup.sh is the live example).
#
# The fixture artifact is a COPY of the real one under a private
# CARGO_TARGET_DIR. The real artifact is never touched: three or four
# concurrent worktree suites share it, and renaming it away would be exactly
# the interference this test exists to prove harmless.
source "$(dirname "$0")/lib.sh"

# shellcheck source=../../../scripts/binary-lease.sh
. "$TESTS_DIR/../../../scripts/binary-lease.sh"

repo_root="$(cd "$TESTS_DIR/../../.." && pwd -P)"
real_artifact="${CARGO_TARGET_DIR:-$repo_root/target}/debug/story"

# --- 3. a nested instance reuses the outer lease ----------------------------
# Checked first, against THIS test's own lease: the outer instance is the one
# whose home and daemon the nested `bash -c` shares.
outer_resolved="$(command -v story)"
lease_root="$(dirname "$(dirname "$outer_resolved")")"
assert_eq "$(basename "$lease_root")" "$STORYHOOK_BINARY_LEASE_DIR" \
  "lease: this test's own \`story\` resolves under the lease root"
case "$(basename "$(dirname "$outer_resolved")")" in
  "$$"-*) : ;;
  *) fail_test "lease: this test's lease is not owned by this test's pid $$: $outer_resolved" ;;
esac

before="$(ls -1d "$lease_root"/*/ 2>/dev/null | wc -l | tr -d ' ')"
nested_resolved="$(bash -c 'source "$1"; command -v story' _ "$TESTS_DIR/lib.sh")"
after="$(ls -1d "$lease_root"/*/ 2>/dev/null | wc -l | tr -d ' ')"
assert_eq "$nested_resolved" "$outer_resolved" \
  "lease: a nested instance sharing the home resolves the OUTER lease, not one of its own"
# Counted before and after rather than asserted equal to 1: concurrent suites
# lease the same artifact and their entries sit in the same root.
assert_eq "$after" "$before" \
  "lease: a nested instance minted no lease entry of its own"

# A nested instance runs whichever `story` its caller arranged, and a caller
# may be a harness that installed its own COPY -- a distinct inode the rebuild
# cannot touch (`tests/plugin_install.rs` sources lib.sh exactly so). That copy
# is admitted and resolved; the first cut of this rule demanded the outer lease
# by name and failed two of that file's tests under central verification.
installed="$(mktemp -d /tmp/story-test-lease-installed.XXXXXX)"
_TMP_REPOS+=("$installed")
cp "$real_artifact" "$installed/story"
installed_resolved="$(PATH="$installed:$PATH" bash -c 'source "$1"; command -v story' _ "$TESTS_DIR/lib.sh")"
assert_eq "$installed_resolved" "$installed/story" \
  "lease: a nested instance runs a caller-installed copy of the binary untouched"

# What a nested instance refuses is the one shape that IS the defect: the
# artifact's own inode reached outside the lease root -- the bare
# `target/debug/story` a rebuild replaces under the shared daemon.
bare_dir="$(dirname "$real_artifact")"
if PATH="$bare_dir:$PATH" bash -c 'source "$1"' _ "$TESTS_DIR/lib.sh" >/dev/null 2>"$installed/refusal"; then
  fail_test "lease: a nested instance ran with the BARE artifact first on PATH"
fi
assert_contains "$(cat "$installed/refusal")" "BARE" \
  "lease: the bare-artifact refusal names what it found"

# --- 1 & 2. the lease survives a rebuild, and so does the daemon ------------
fixture="$(mktemp -d /tmp/story-test-lease-target.XXXXXX)"
_TMP_REPOS+=("$fixture")
mkdir -p "$fixture/debug"
cp "$real_artifact" "$fixture/debug/story"
# A fixed, old mtime, so the replacement below provably differs in the one
# field a rebuild changes besides the inode. Without this, a copy made within
# the same second as the original would share its mtime and the control in
# the child would be vacuous.
touch -t 202001010000 "$fixture/debug/story"

report="$(mktemp /tmp/story-test-lease-report.XXXXXX)"
_TMP_REPOS+=("$report")

# A second, independent test process: STORYHOOK_TEST_HOME is unset so lib.sh
# mints a fresh root, leases the FIXTURE artifact and parents a daemon to the
# child. The child exits through lib.sh's own EXIT trap -- the code under test
# for the lease's removal.
env -u STORYHOOK_TEST_HOME -u STORYHOOK_REAL_HOME CARGO_TARGET_DIR="$fixture" bash -c '
  source "$1"
  fixture="$2"; report="$3"
  artifact="$fixture/debug/story"
  resolved="$(command -v story)"
  lease_dir="$(dirname "$resolved")"

  # 1. a lease of the fixture artifact, owned by this child.
  [ "$(basename "$(dirname "$lease_dir")")" = "$STORYHOOK_BINARY_LEASE_DIR" ] || exit 91
  [ "$(dirname "$(dirname "$lease_dir")")" -ef "$fixture/debug" ] || exit 92
  case "$(basename "$lease_dir")" in "$$"-*) : ;; *) exit 93 ;; esac
  [ "$resolved" -ef "$artifact" ] || exit 94

  repo=$(mk_story_repo)
  (cd "$repo" && story list >/dev/null) || exit 95
  portfile=$(ls "$STORYHOOK_TEST_HOME"/home/.local/state/storyhook/daemons/*/daemon.json 2>/dev/null | head -1)
  [ -n "$portfile" ] || exit 96
  pid_before=$(jq -r .pid "$portfile")
  # The daemon runs the lease, so its recorded exe is the leased inode.
  [ "$(jq -r .exe "$portfile")" -ef "$resolved" ] || exit 97

  # 2. replace the artifact the way Cargo does: a new inode, renamed over
  #    the entry. The fixture control: the replacement is a different inode
  #    with a different mtime, or the assertion below proves nothing.
  cp "$artifact" "$artifact.tmp" && mv "$artifact.tmp" "$artifact" || exit 98
  [ "$resolved" -ef "$artifact" ] && exit 81
  mtime_lease=$(stat -f %m "$resolved" 2>/dev/null || stat -c %Y "$resolved")
  mtime_new=$(stat -f %m "$artifact" 2>/dev/null || stat -c %Y "$artifact")
  [ "$mtime_lease" != "$mtime_new" ] || exit 82

  # `story` still resolves the leased inode and still runs...
  [ "$(command -v story)" = "$resolved" ] || exit 83
  story --version >/dev/null 2>&1 || exit 84
  # ...and the next call reaches the SAME daemon rather than replacing it.
  (cd "$repo" && story list >/dev/null) || exit 85
  pid_after=$(jq -r .pid "$portfile")
  [ "$pid_after" = "$pid_before" ] || exit 86
  [ "$(jq -r .exe "$portfile")" -ef "$resolved" ] || exit 87

  # Positive control: the replaced artifact IS a different identity. Calling
  # it directly stands the leased daemon down and seats itself -- the exact
  # restart the lease exists to prevent -- so the pid assertion above is
  # proven able to see one. Teardown stops whichever daemon holds the pidfile;
  # `daemon stop` authenticates by the portfile, never by binary identity.
  (cd "$repo" && "$artifact" list >/dev/null) || exit 88
  [ "$(jq -r .pid "$portfile")" != "$pid_before" ] || exit 89

  # The daemon teardown is about to stop is the control daemon, so that is
  # the pid the parent watches.
  printf "%s\n%s\n%s\n" "$STORYHOOK_TEST_HOME" "$(jq -r .pid "$portfile")" "$lease_dir" >"$report"
' _ "$TESTS_DIR/lib.sh" "$fixture" "$report"
child_status=$?
case "$child_status" in
  0) : ;;
  81) fail_test "lease: fixture control -- the replacement kept the artifact's inode" ;;
  82) fail_test "lease: fixture control -- the replacement kept the artifact's mtime" ;;
  83) fail_test "lease: after the rebuild \`story\` no longer resolves the lease" ;;
  84) fail_test "lease: the leased binary no longer runs after the rebuild" ;;
  85) fail_test "lease: \`story list\` failed after the rebuild" ;;
  86) fail_test "lease: the test's daemon was RESTARTED by the rebuild -- its pid changed" ;;
  87) fail_test "lease: the daemon answering after the rebuild does not run the lease" ;;
  88) fail_test "lease: positive control -- the bare replaced artifact failed to run" ;;
  89) fail_test "lease: positive control -- the bare replaced artifact did NOT restart the daemon, so the pid assertion proves nothing" ;;
  91 | 92 | 93) fail_test "lease: \`story\` does not resolve to a lease owned by the child beside the fixture artifact (exit $child_status)" ;;
  94) fail_test "lease: the child's lease does not share the fixture artifact's inode" ;;
  97) fail_test "lease: the child's daemon does not run the leased inode" ;;
  *) fail_test "lease: child test process failed with exit $child_status" ;;
esac

child_home=$(sed -n 1p "$report")
child_daemon=$(sed -n 2p "$report")
child_lease_dir=$(sed -n 3p "$report")
if [ "$child_status" -eq 0 ]; then
  [ -n "$child_home" ] && [ -n "$child_daemon" ] && [ -n "$child_lease_dir" ] \
    || fail_test "lease: child did not report its home, daemon pid and lease"

  # Teardown stopped the daemon and only THEN released the lease it ran: the
  # stop needs `story` on PATH, so the lease outlives the daemon, never the
  # other way round.
  if kill -0 "$child_daemon" 2>/dev/null; then
    fail_test "lease: child's daemon (pid $child_daemon) outlived the child"
  fi
  [ ! -e "$child_lease_dir" ] \
    || fail_test "lease: child's lease survived its EXIT trap at $child_lease_dir"
  leftover="$(ls -1d "$fixture/debug/$STORYHOOK_BINARY_LEASE_DIR"/*/ 2>/dev/null | wc -l | tr -d ' ')"
  assert_eq "$leftover" "0" "lease: no lease entry left under the fixture after the child exited"
  [ ! -e "$child_home" ] \
    || fail_test "lease: child's home survived its EXIT trap at $child_home"
fi

finish
