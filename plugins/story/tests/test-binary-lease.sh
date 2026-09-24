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
# Four invariants, each measured -- against a REAL daemon, or against what the
# helper actually executes -- rather than inferred from lib.sh's text:
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
#   4. An inherited STORY_BIN never outranks the lease (SH-764). story.sh and
#      github-access.sh run `${STORY_BIN:-story}`, and a dispatched agent
#      session exports the installed release as STORY_BIN, so `command -v
#      story` alone says nothing about what the helper runs. The owning
#      instance removes it; a nested instance refuses one that is not the
#      `story` its caller put on PATH.
#
# The fixture artifact is a COPY of the real one under a private
# CARGO_TARGET_DIR. The real artifact is never replaced or renamed -- only
# leased, which is a hard link every owning instance takes: three or four
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

# --- 4. an inherited STORY_BIN never outranks the lease (SH-764) ------------
# The decoy records every run in `ran`, so "the decoy never ran" is observed
# rather than inferred from what the helper printed.
decoy="$(mktemp -d /tmp/story-test-lease-decoy.XXXXXX)"
_TMP_REPOS+=("$decoy")
cat >"$decoy/story" <<'DECOY'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$(dirname "$0")/ran"
printf 'story-decoy 0.0.0\n'
DECOY
chmod +x "$decoy/story"

# An owning instance, in the shape of a Rust wrapper that spawns a plugin test
# with STORYHOOK_TEST_HOME removed and everything else inherited
# (`src/service/verification.rs` does exactly this). `ensure-cli` is the
# helper's own `"$STORY" --version`, so its answer is what the helper runs.
# Lines: whether STORY_BIN is set at all, `.installed`, the helper's version,
# the lease's version. An optional second argument is exported AFTER sourcing:
# the shape of the tests that install a fake on purpose.
owner_probe='
  source "$1"
  printf "%s\n" "${STORY_BIN+set}"
  [ -z "${2:-}" ] || export STORY_BIN="$2"
  answer="$(bash "$SCRIPT" ensure-cli)" || exit 71
  printf "%s\n" "$(printf "%s" "$answer" | jq -r .installed)"
  printf "%s\n" "$(printf "%s" "$answer" | jq -r .version)"
  story --version
'
# Empty is the value tmux-launch.py hands a pane that has no binary of its own.
for inherited in "$decoy/story" ""; do
  label="an inherited STORY_BIN=[$inherited]"
  seen="$(env -u STORYHOOK_TEST_HOME -u STORYHOOK_REAL_HOME STORY_BIN="$inherited" \
    bash -c "$owner_probe" _ "$TESTS_DIR/lib.sh")"
  rc=$?
  assert_eq "$rc" "0" "story-bin: an owning instance under $label runs the helper"
  assert_eq "$(sed -n 1p <<<"$seen")" "" \
    "story-bin: an owning instance removes $label rather than leaving it set"
  assert_eq "$(sed -n 2p <<<"$seen")" "true" \
    "story-bin: under $label the helper finds a binary to run"
  assert_eq "$(sed -n 3p <<<"$seen")" "$(sed -n 4p <<<"$seen")" \
    "story-bin: under $label the helper runs the lease, not the inherited binary"
done
[ ! -e "$decoy/ran" ] \
  || fail_test "story-bin: the inherited decoy ran in place of the lease: $(cat "$decoy/ran")"

# Positive control: a STORY_BIN the test itself sets after sourcing lib.sh IS
# what the helper runs. This proves the probe above can see a decoy run, and
# that the tests which install a fake on purpose still get it.
seen="$(env -u STORYHOOK_TEST_HOME -u STORYHOOK_REAL_HOME \
  bash -c "$owner_probe" _ "$TESTS_DIR/lib.sh" "$decoy/story")"
assert_eq "$(sed -n 3p <<<"$seen")" "story-decoy 0.0.0" \
  "story-bin: positive control -- a STORY_BIN set after sourcing is what the helper runs"
[ -e "$decoy/ran" ] \
  || fail_test "story-bin: positive control -- the decoy never ran, so its absence above proves nothing"

# A nested instance shares its caller's daemon, and daemon identity is the
# exe PATH string, so STORY_BIN must name exactly the `story` its caller put
# on PATH. The bare artifact is the same inode as the lease at a second path:
# refused by the STORY_BIN rule, not by the PATH rule's BARE message.
for bad in "$decoy/story" /nonexistent/story "$real_artifact"; do
  if STORY_BIN="$bad" bash -c 'source "$1"' _ "$TESTS_DIR/lib.sh" >/dev/null 2>"$decoy/refusal"; then
    fail_test "story-bin: a nested instance ran with STORY_BIN=[$bad], which is not the \`story\` on its PATH"
  fi
  refusal="$(cat "$decoy/refusal")"
  assert_contains "$refusal" "STORY_BIN" "story-bin: the nested refusal of STORY_BIN=[$bad] names the variable"
  case "$refusal" in
    *BARE*) fail_test "story-bin: STORY_BIN=[$bad] was refused by the PATH rule, not the STORY_BIN rule: $refusal" ;;
  esac
done

# What a nested instance keeps: a STORY_BIN that names the caller's own
# `story`, by path or by name, an empty one, and a caller-installed copy that
# PATH and STORY_BIN both name. Printed back to prove it was kept, not dropped.
nested_probe='source "$1"; printf "%s" "${STORY_BIN-<unset>}"'
for good in "$outer_resolved" story ""; do
  kept="$(STORY_BIN="$good" bash -c "$nested_probe" _ "$TESTS_DIR/lib.sh" 2>"$decoy/refusal")" \
    || fail_test "story-bin: a nested instance refused STORY_BIN=[$good], which names its caller's \`story\`: $(cat "$decoy/refusal")"
  assert_eq "$kept" "$good" "story-bin: a nested instance keeps its caller's STORY_BIN=[$good]"
done
kept="$(PATH="$installed:$PATH" STORY_BIN="$installed/story" \
  bash -c "$nested_probe" _ "$TESTS_DIR/lib.sh" 2>"$decoy/refusal")" \
  || fail_test "story-bin: a nested instance refused a caller-installed copy named by both PATH and STORY_BIN: $(cat "$decoy/refusal")"
assert_eq "$kept" "$installed/story" \
  "story-bin: a nested instance keeps a caller-installed STORY_BIN that matches its PATH"

finish
