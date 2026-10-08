#!/usr/bin/env bash
# Shared helpers for story.sh tests. Source this at the top of each
# test-*.sh. Modelled on agentics' plugins/issue/tests/lib.sh, adapted for
# storyhook: unlike agentics (which doesn't bundle storyhook and must fake
# the `story` CLI), this repo builds the REAL binary, so fixtures use it
# directly rather than a scripted double -- a fake can't catch a genuine
# CAS race or a real is_ready() interaction, exactly the class of thing
# the ready-gate and already-in-progress guard exist to get right.
set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# HOST ADMISSION (SH-869). A test script run on its own is a `plugin-script`
# entry. Only a top-level `test-*.sh` that sources this file itself is
# re-executed through the adapter, before anything below runs: a fixture that
# sources it from `bash -c`, a helper, or the fake tmux is not. Under the
# plugin runner, or any other admitted entry, the script already runs inside
# that grant, where the adapter would run it in place.
if [ "${BASH_SOURCE[1]:-}" = "$0" ] && [ -z "${STORYHOOK_HOST_ENTRY:-}" ]; then
  case "$(basename -- "$0")" in
  (test-*.sh)
    . "$TESTS_DIR/../../../scripts/python-runtime.sh" || exit 2
    storyhook_python_init || { printf '%s\n' "$STORYHOOK_PYTHON_ERROR" >&2; exit 2; }
    exec "$STORYHOOK_PYTHON" -B "$TESTS_DIR/../../../scripts/host-admit.py" \
      --entry plugin-script -- bash "$0" "$@"
    ;;
  esac
fi
PLUGIN_ROOT="$(cd "$TESTS_DIR/.." && pwd)"
SCRIPT="$PLUGIN_ROOT/bin/story.sh"

# Return one shipped handler by event, matcher and quoted script path. Tests
# execute its actual command; array order must never choose another behavior.
manifest_hook() {
  jq -ce --arg event "$1" --arg matcher "$2" --arg script "$3" '
    [(.hooks[$event] // [])[] | select(.matcher == $matcher) | .hooks[]
      | select(.type == "command")
      | select(.command | endswith("/hooks/" + $script + "\""))]
    | if length == 1 then .[0]
      else error("expected exactly one \($event) / \($matcher) / \($script) handler; found \(length)")
      end
  ' "${4:-$PLUGIN_ROOT/hooks/hooks.json}"
}

# Loaded unconditionally: run-tests.sh has already built the test environment,
# but every Git this file and its callers run still needs the shared constructor.
# shellcheck source=../../../scripts/test-env.sh
. "$TESTS_DIR/../../../scripts/test-env.sh"

# Every test file sources this library, so this one wrapper routes its fixture
# setup, assertions, and mutations through the allowlisted constructor without
# a hand-maintained list of call sites. Child Bash processes do not inherit the
# function; the plugin under test therefore still exercises production Git.
git() {
  storyhook_fixture_git "$@"
}

_TMP_REPOS=()
_TMP_REPOS_MANIFEST="$(mktemp /tmp/story-test-cleanup.XXXXXX)"
_TMP_TMUX_SESSIONS=()

# A helper called through `path=$(helper)` runs in a subshell, so appending to
# the caller's Bash array cannot register anything for its EXIT trap. This
# manifest is the cross-process ownership record for those returned paths.
_register_tmp() {
  local d
  for d in "$@"; do
    [ -n "$d" ] && printf '%s\n' "$d" >>"$_TMP_REPOS_MANIFEST"
  done
}

# Register one test-owned tmux session for exact cleanup at process exit. The
# namespace guard keeps a mistaken call from ever claiming a persistent user or
# project session; callers still provide the concrete name so concurrent test
# sessions cannot be caught by a broad prefix sweep.
_register_tmp_tmux_session() {
  local session
  # A real server is about to take this test's default path (SH-840).
  _withdraw_fake_tmux_default || return 1
  for session in "$@"; do
    case "$session" in
      story-test-*) _TMP_TMUX_SESSIONS+=("$session") ;;
      *)
        printf 'refusing to register unowned test tmux session: %s\n' "$session" >&2
        return 1
        ;;
    esac
  done
}

_cleanup() {
  local status=$? cleanup_failed=0 d session session_status
  trap - EXIT

  # The daemon this test owns is stood down BEFORE its home is deleted, the
  # rule `TestEnv::stop_daemon` already states for the Rust suite ("a test
  # that asks about bytes on disk must stand the daemon down first"). Deleting
  # the home first left a live daemon holding an unlinked store; it noticed its
  # parent had gone up to one SHUTDOWN_CHECK (250ms) later, and its exit
  # journal recreated the directory it was writing into -- 77 resurrected
  # fixture roots in /tmp on the machine that filed SH-631, and the exact
  # window the gate postlude reports as "a daemon serving a store that no
  # longer exists". Only the process that MINTED the home owns the daemon; a
  # nested `bash -c 'source lib.sh'` shares its caller's and must not stop it.
  # `--force`: a stop that waits for ever on a wedged daemon is a wedged suite
  # (SH-528), and a daemon that cannot be stopped is a leak -- reported as a
  # failed test rather than left for the next run to find (SH-306).
  if [ "${_STORYHOOK_OWNS_TEST_HOME:-0}" = 1 ]; then
    if ! story daemon stop --force >/dev/null 2>&1; then
      printf 'failed to stop the test daemon under %s before deleting it\n' \
        "$STORYHOOK_TEST_HOME" >&2
      cleanup_failed=1
    fi
  fi

  if [ "${#_TMP_TMUX_SESSIONS[@]}" -gt 0 ]; then
    # The engine can still be between claiming a lane and creating its terminal.
    # Stop it before reaping the registered sessions so cleanup cannot race a
    # late creation after the absence check.
    if ! story daemon stop --force >/dev/null 2>&1; then
      printf 'failed to stop isolated test daemon before tmux cleanup\n' >&2
      cleanup_failed=1
    fi

    for session in "${_TMP_TMUX_SESSIONS[@]}"; do
      tmux has-session -t "=$session" 2>/dev/null
      session_status=$?
      case "$session_status" in
        0)
          if ! tmux kill-session -t "=$session" >/dev/null 2>&1; then
            printf 'failed to kill registered test tmux session: %s\n' "$session" >&2
            cleanup_failed=1
          fi
          ;;
        1) : ;;
        *)
          printf 'failed to query registered test tmux session: %s\n' "$session" >&2
          cleanup_failed=1
          ;;
      esac

      tmux has-session -t "=$session" 2>/dev/null
      session_status=$?
      case "$session_status" in
        0)
          printf 'registered test tmux session survived cleanup: %s\n' "$session" >&2
          cleanup_failed=1
          ;;
        1) : ;;
        *)
          printf 'could not verify test tmux session cleanup: %s\n' "$session" >&2
          cleanup_failed=1
          ;;
      esac
    done
  fi

  # Fake panes deliberately outlive their launch commands. Their native
  # ledger is separate from mutable pane_pid/state files and covers every
  # overridden state directory. Nested sources never retire their caller's
  # ledger. Refusal must preserve all paths a surviving writer might borrow.
  if [ -n "${_STORYHOOK_FAKE_SCOPE:-}" ]; then
    if ! python3 -B "$TESTS_DIR/fake-process-owner.py" cleanup-scope \
        "$FAKE_TMUX_PROCESS_LEDGER" "$_STORYHOOK_FAKE_SCOPE" "$$"; then
      printf 'fake scope cleanup uncertain; retaining all fixture roots: %s\n' \
        "$FAKE_TMUX_PROCESS_LEDGER" >&2
      [ "$status" -ne 0 ] || status=1
      exit "$status"
    fi
  fi
  if [ "${_STORYHOOK_OWNS_FAKE_PROCESSES:-0}" = 1 ]; then
    if ! python3 -B "$TESTS_DIR/fake-process-owner.py" cleanup "$FAKE_TMUX_PROCESS_LEDGER" "$$"; then
      printf 'fake process cleanup uncertain; retaining fixture roots and ledger %s\n' \
        "$FAKE_TMUX_PROCESS_LEDGER" >&2
      [ "$status" -ne 0 ] || status=1
      exit "$status"
    fi
  fi
  # A hook admitted before the first stop can finish a request while its
  # publisher drains. With every owned writer now settled, the owning fixture
  # closes that final daemon-start race before deleting its home.
  if [ "${_STORYHOOK_OWNS_TEST_HOME:-0}" = 1 ]; then
    if ! story daemon stop --force >/dev/null 2>&1; then
      printf 'failed to stop the test daemon after fixture writers settled\n' >&2
      cleanup_failed=1
    fi
  fi
  if [ "$cleanup_failed" -ne 0 ]; then
    printf 'fixture cleanup failed; retaining all temporary roots\n' >&2
    [ "$status" -ne 0 ] || status=1
    exit "$status"
  fi

  for d in "${_TMP_REPOS[@]:-}"; do
    [ -n "$d" ] && rm -rf -- "$d"
  done
  while IFS= read -r d; do
    case "$d" in
      /tmp/story-test.* | /tmp/story-test-origin.* | /tmp/story-test-claude.*)
        rm -rf -- "$d"
        ;;
      *)
        printf 'refusing to clean unowned test path: %s\n' "$d" >&2
        ;;
    esac
  done <"$_TMP_REPOS_MANIFEST"
  rm -f -- "$_TMP_REPOS_MANIFEST"

  [ "$status" -eq 0 ] || exit "$status"
  [ "$cleanup_failed" -eq 0 ] || exit 1
}
trap _cleanup EXIT

# --- data-home isolation ---------------------------------------------------
#
# storyhook keeps every project's stories in a single store under the XDG data
# home. A suite that has not redirected these variables writes its fixtures
# into the developer's real ~/.local/share/storyhook on every `make test` --
# hundreds of junk stories in the tracker this project uses to track itself,
# with no undo and nothing in the output to say it happened. This block was
# written before the store landed, while the variables were still unread,
# because adding it afterwards would have been adding it too late.
#
# This block is the ONE isolation every test gets, under run-tests.sh and on
# its own alike (SH-631): a root of its own, a daemon of its own, and
# STORYHOOK_PARENT_PID = this process, so the daemon dies with the test by
# construction rather than with the run. run-tests.sh isolates only itself and
# deliberately leaves $STORYHOOK_TEST_HOME unset so this branch runs for every
# test; the variable being set means an OUTER lib.sh instance owns the home --
# a nested `bash -c 'source lib.sh'` (test-temp-cleanup.sh) shares its caller's
# store and daemon and must not mint, stop or delete anything of its own.
# STORYHOOK_REAL_HOME survives so a test can assert the real data home was
# left alone; an inherited value wins, because run-tests.sh has already
# rewritten $HOME by the time this runs.
if [ -z "${STORYHOOK_TEST_HOME:-}" ]; then
  # A fresh fixture owns no pane in the operator's server. TMUX outranks
  # TMUX_TMPDIR in resource discovery, so isolating only the latter still
  # queried the host when this script was launched inside tmux (SH-888).
  # Nested instances retain selectors deliberately installed by their owner.
  unset TMUX TMUX_PANE
  export STORYHOOK_REAL_HOME="${STORYHOOK_REAL_HOME:-$HOME}"
  STORYHOOK_TEST_HOME="$(mktemp -d /tmp/storyhook-plugin-home.XXXXXX)"
  export STORYHOOK_TEST_HOME
  _STORYHOOK_OWNS_TEST_HOME=1
  _TMP_REPOS+=("$STORYHOOK_TEST_HOME")

  # THE ISOLATION, in one shared place -- `scripts/test-env.sh`, whose own
  # header carries the parameters and the reason for each. `--home` IS passed:
  # this suite runs nothing but `story` and `git`.
  storyhook_isolate --home "$STORYHOOK_TEST_HOME"
  # The native CLI/daemon probes fake subprocesses too. Declare patience
  # before the first story starts the daemon; nested libraries retain it.
  if ! _STORY_PROBE_SECONDS="$(python3 "$TESTS_DIR/../../../scripts/tests/load_grace.py" patience 30)"; then
    printf 'lib.sh: cannot grace the native subprocess probe budget\n' >&2
    exit 1
  fi
  export STORYHOOK_TEST_SUBPROCESS_PATIENCE_MS="$((_STORY_PROBE_SECONDS * 1000))"
  unset _STORY_PROBE_SECONDS

  # A standalone `bash test-foo.sh` (this branch) has no SH-524 progress
  # journal of its own to write to; an ambient one set by some other daemon-
  # owned run must not be inherited and mistaken for this test's. Not a
  # test-environment parameter -- a harness legitimately SETS this one -- so it
  # stays here rather than joining the shared table.
  unset STORYHOOK_GATE_PROGRESS
fi

# Read-only native resource queries must never inspect the operator's tmux
# server. A caller outside tmux is pointed at its DEFAULT server,
# $TMUX_TMPDIR/tmux-<uid>/default, by tmux and, since SH-825 (90a4a55a), by the
# helper's own socket selection -- so an unset TMUX_TMPDIR means the real one
# under /tmp. This used to be set only by the instance that owns the home, and a
# nested instance under a harness that clears its environment
# (tests/support/protect_*.rs) inherited none: its unclaim asked the machine's
# real default server and was refused as an unowned socket (SH-840). Every
# instance therefore keeps the directory inside the test's own home.
case "${TMUX_TMPDIR:-}" in
  "$STORYHOOK_TEST_HOME"/*) ;;
  *) export TMUX_TMPDIR="$STORYHOOK_TEST_HOME/tmux" ;;
esac
mkdir -p "$TMUX_TMPDIR"

# --- the binary under test -------------------------------------------------
#
# THIS SUITE RUNS `story` BY NAME, so without this block it runs whichever one
# `$PATH` happens to reach -- and off `make test`, that is the developer's
# INSTALLED build. The store is isolated either way, so nothing is damaged and
# nothing is reported: the wrong binary is simply exercised, and its failures
# read as product bugs.
#
# Not hypothetical. Found while SH-531 was being written: `bash
# plugins/story/tests/run-tests.sh`, typed by hand, failed test-reap.sh and
# test-dispatch-epic.sh against an installed v2.2.0 whose `default_states()`
# predates the `verifying` state. The error was `state \`verifying\` not found`,
# which names a state, a project and a store -- and not the one thing that was
# actually wrong.
#
# `make test` used to supply this too (`Makefile`'s plugin leg prepended
# `target/debug` until SH-639), which is exactly why it went unnoticed: the
# gate was right and the standalone path was silently testing something else.
# This is the SH-226 shape one layer over -- what a process IS, rather than
# what a `$PATH` happens to resolve. The artifact is resolved HERE, from the
# checkout, and never read off `$PATH`; `make test` and a hand-typed
# `bash test-foo.sh` are one code path (SH-631).
#
# WHAT GOES ON `$PATH` IS A LEASE OF THE ARTIFACT, NEVER THE ARTIFACT (SH-639).
# Cargo replaces `target/debug/story` by writing a new inode and renaming it
# over the entry, and a daemon's identity is `(version, exe, exe_mtime)`
# (`DaemonInfo::is_this_binary`) -- so with the bare artifact on `$PATH`, any
# `cargo build|test|check` in the checkout landing mid-test changed the
# identity of the test's own daemon, and the next `story` call stood it down
# and restarted it on a fresh port, silently. `scripts/binary-lease.sh` (the
# shell rendering of SH-532's `story_binary()`, given to the browser runner by
# SH-635) hard-links the artifact into `<artifact dir>/.storyhook-test-binaries/
# <pid>-<nonce>/story`, so the inode this test started with stays alive and
# unchanged for as long as the link does. The owner is `$$` -- this test --
# for the reason its daemon is (SH-631): the lease dies with the test, and the
# sweeper reclaims one whose owner is provably gone.
#
# ONLY THE INSTANCE THAT OWNS THE HOME LEASES. A nested `bash -c 'source
# lib.sh'` (test-temp-cleanup.sh) shares its caller's store AND daemon, and
# identity is a PATH compare: a second lease of the same inode at a second
# path would make the nested instance's first `story` call stand the shared
# daemon down -- the very restart this block exists to prevent. The nested
# instance changes nothing about `$PATH`: it runs whichever `story` its caller
# arranged -- the outer lease, or a harness's own installed copy
# (`tests/plugin_install.rs` sources this file under an inherited home with a
# fixture `bin/story` first on `$PATH`, a distinct inode the rebuild cannot
# touch). What it refuses, by name, is the one shape that IS the defect: the
# artifact's own inode reached at a path outside the lease root, which is the
# bare `target/debug/story` or a symlink to it.
#
# The lease directory is registered for `_cleanup`, which removes it AFTER
# `story daemon stop --force`: the stop needs `story` on `$PATH`, so the lease
# must outlive the daemon, never the other way round.
#
# Prepended rather than replacing `$PATH`: the suite needs `git`, `jq` and the
# fake tmux, and a test file's own `PATH="$TESTS_DIR/fakes:$PATH"` still wins
# over this for the names it provides.
#
# `$PATH` IS THE ONLY SELECTOR (SH-764). story.sh and github-access.sh run
# `${STORY_BIN:-story}`, so an inherited STORY_BIN outranks the lease -- and a
# dispatched agent session exports the installed release as STORY_BIN, which
# made every plugin test run from one exercise that release through the helper.
# The owning instance therefore REMOVES it rather than pinning it to the lease:
# several tests put a proxy `story` first on `$PATH` (fakes/story-verifying and
# its siblings) and depend on the helper's fallback reaching it. A test that
# wants a fake STORY_BIN sets it after sourcing this file. A nested instance
# keeps an inherited STORY_BIN only when it names exactly the `story` its caller
# put on `$PATH`: the daemon they share identifies its binary by path string, so
# a second spelling -- the bare artifact beside its lease, a relative name --
# would restart it, and a foreign binary would be tested in its place.
# shellcheck source=../../../scripts/binary-lease.sh
. "$TESTS_DIR/../../../scripts/binary-lease.sh"
_STORY_ARTIFACT="$(storyhook_debug_artifact "$(cd "$TESTS_DIR/../../.." && pwd)")"
_STORY_TARGET_DIR="${_STORY_ARTIFACT%/debug/story}"
unset _STORY_ARTIFACT
if [ ! -x "$_STORY_TARGET_DIR/debug/story" ]; then
  echo "refusing to run: $_STORY_TARGET_DIR/debug/story does not exist." >&2
  echo "  This suite tests the \`story\` THIS checkout builds, never the one" >&2
  echo "  installed on the machine -- an installed binary is a different" >&2
  echo "  version whose failures read as product bugs. Run \`cargo build --features test-seam\`" >&2
  echo "  first, or \`make test\`, which does." >&2
  exit 1
fi
if [ "${_STORYHOOK_OWNS_TEST_HOME:-0}" = 1 ]; then
  _STORY_LEASE="$(storyhook_lease_binary "$_STORY_TARGET_DIR/debug/story")" || exit 1
  _STORY_LEASE_DIR="$(dirname "$_STORY_LEASE")"
  _TMP_REPOS+=("$_STORY_LEASE_DIR")
  export PATH="$_STORY_LEASE_DIR:$PATH"
  unset STORY_BIN
  unset _STORY_LEASE _STORY_LEASE_DIR
else
  _STORY_INHERITED="$(command -v story || true)"
  if [ -z "$_STORY_INHERITED" ]; then
    echo "refusing to run: this lib.sh instance inherited \$STORYHOOK_TEST_HOME," >&2
    echo "  so it shares its caller's daemon and runs the caller's \`story\`," >&2
    echo "  but nothing on \$PATH resolves that name." >&2
    exit 1
  fi
  if [ "$_STORY_INHERITED" -ef "$_STORY_TARGET_DIR/debug/story" ]; then
    case "$_STORY_INHERITED" in
      "$_STORY_TARGET_DIR/debug/$STORYHOOK_BINARY_LEASE_DIR"/*/story) : ;;
      *)
        echo "refusing to run: this lib.sh instance inherited \$STORYHOOK_TEST_HOME," >&2
        echo "  so it shares its caller's daemon, but \`story\` resolves to the BARE" >&2
        echo "  Cargo artifact [$_STORY_INHERITED] rather than a lease of it under" >&2
        echo "  $_STORY_TARGET_DIR/debug/$STORYHOOK_BINARY_LEASE_DIR/. A rebuild" >&2
        echo "  would replace it under the shared daemon (SH-639)." >&2
        exit 1
        ;;
    esac
  fi
  if [ -n "${STORY_BIN:-}" ]; then
    _STORY_SELECTED="$(command -v "$STORY_BIN" || true)"
    if [ -z "$_STORY_SELECTED" ]; then
      echo "refusing to run: this lib.sh instance inherited \$STORYHOOK_TEST_HOME and" >&2
      echo "  STORY_BIN [$STORY_BIN], which the helper runs in place of \`story\`" >&2
      echo "  (\${STORY_BIN:-story}), but STORY_BIN does not resolve to a program." >&2
      echo "  Unset it, or name the caller's \`story\` [$_STORY_INHERITED] (SH-764)." >&2
      exit 1
    fi
    if [ "$_STORY_SELECTED" != "$_STORY_INHERITED" ]; then
      echo "refusing to run: this lib.sh instance inherited \$STORYHOOK_TEST_HOME, so it" >&2
      echo "  shares its caller's daemon and runs the \`story\` on \$PATH [$_STORY_INHERITED]," >&2
      echo "  but it also inherited STORY_BIN, which the helper runs in its place" >&2
      echo "  (\${STORY_BIN:-story}), and that resolves to [$_STORY_SELECTED]. The daemon" >&2
      echo "  identifies its binary by path, so both must name the same one. Unset" >&2
      echo "  STORY_BIN, or set it to exactly [$_STORY_INHERITED] (SH-764)." >&2
      exit 1
    fi
    unset _STORY_SELECTED
  fi
  unset _STORY_INHERITED
fi
unset _STORY_TARGET_DIR

# The scope's native process and writer settlement is patient under the same
# bounded load policy as its pane. A test may explicitly exercise refusal with
# a shorter allowance; this never shortens the pane's own lifetime.
if [ -z "${FAKE_TMUX_CLEANUP_SECONDS:-}" ]; then
  FAKE_TMUX_CLEANUP_SECONDS="$(python3 "$TESTS_DIR/../../../scripts/tests/load_grace.py" patience 30)" || exit 1
  export FAKE_TMUX_CLEANUP_SECONDS
fi

# Only the instance minting a process ledger owns its teardown. A nested
# library shares registration but cannot adopt cleanup ownership.
if [ "${_STORYHOOK_OWNS_TEST_HOME:-0}" = 1 ] || [ -z "${FAKE_TMUX_PROCESS_LEDGER:-}" ]; then
  FAKE_TMUX_PROCESS_LEDGER="$(mktemp -d /tmp/story-test-processes.XXXXXX)" || exit 1
  export FAKE_TMUX_PROCESS_LEDGER
  python3 -B "$TESTS_DIR/fake-process-owner.py" init "$FAKE_TMUX_PROCESS_LEDGER" "$$" || exit 1
  _STORYHOOK_OWNS_FAKE_PROCESSES=1
  _TMP_REPOS+=("$FAKE_TMUX_PROCESS_LEDGER")
else
  _STORYHOOK_OWNS_FAKE_PROCESSES=0
fi

_STORYHOOK_FAKE_SCOPE="$(python3 -B "$TESTS_DIR/fake-process-owner.py" scope \
  "$FAKE_TMUX_PROCESS_LEDGER" "$$")" || exit 1
export FAKE_TMUX_PROCESS_SCOPE="$_STORYHOOK_FAKE_SCOPE"

# --- fake-tmux state isolation ---------------------------------------------
#
# fakes/tmux keeps its whole model -- the input buffer, the `launched` flag, the
# derived occupant, the pane pid, the absorb counter -- in files under
# $FAKE_TMUX_STATE, because it is re-exec'd per call and can hold nothing in
# memory. Two users of one directory corrupt each other: one's `new-window`
# clears the other's `launched` and `input`, whose next Enter is then read as a
# launch of nothing and writes a shell name over the first's occupant. That is
# how test-dispatch-auto.sh came to fail a readiness gate against a pane it had
# itself launched `claude` into (SH-263).
#
# It is minted HERE, and not in each test file, for the reason the data-home
# block above it is: the fake used to default to a fixed shared path, five test
# files relied on that default, and a fixture you can forget is one that will be
# forgotten again. Tests needing a FRESH directory per case (a second dispatch
# that must not see the first's state) still mint their own and override this;
# the fake refuses outright if neither did.
if [ -z "${FAKE_TMUX_STATE:-}" ]; then
  FAKE_TMUX_STATE="$(mktemp -d /tmp/story-test-tmux.XXXXXX)"
  export FAKE_TMUX_STATE
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
fi

# rust_duration_secs <path-from-repo-root> <CONST>: the whole seconds of a
# `pub const CONST: Duration = Duration::from_secs(N);` declaration, so a
# fixture derives its bound from the production value (SH-672, SH-766).
# Prints nothing and returns 1 when the declaration moved.
rust_duration_secs() {
  local seconds
  seconds=$(sed -n "s/^pub const $2: Duration = Duration::from_secs(\([0-9]*\));/\1/p" "$TESTS_DIR/../../../$1")
  [ -n "$seconds" ] && printf '%s\n' "$seconds"
}

# A fake pane must survive the bounded dispatch it hosts. Gracing an unrelated
# 30 s lifetime still killed valid Plan retries and trust handshakes before
# DISPATCH_TIMEOUT (SH-814). Derive the base, then apply the shared load grace;
# explicit expiry tests and longer-lived fixtures keep their chosen lifetime.
if [ -z "${FAKE_TMUX_PANE_LIFETIME:-}" ]; then
  if ! _STORY_DISPATCH_SECONDS="$(rust_duration_secs src/service/engine.rs DISPATCH_TIMEOUT)"; then
    printf 'lib.sh: cannot derive DISPATCH_TIMEOUT from src/service/engine.rs\n' >&2
    exit 1
  fi
  if ! FAKE_TMUX_PANE_LIFETIME="$(python3 "$TESTS_DIR/../../../scripts/tests/load_grace.py" patience "$_STORY_DISPATCH_SECONDS")"; then
    printf 'lib.sh: cannot grace the fake pane lifetime (scripts/tests/load_grace.py)\n' >&2
    exit 1
  fi
  export FAKE_TMUX_PANE_LIFETIME
  unset _STORY_DISPATCH_SECONDS
fi

# Keep every terminal operation on the fixture's server. SH-655 found
# fourteen tests consulting the real server through the former census gate.
# SH-672 removes that gate; recovery still probes panes, so isolation remains
# necessary. Per-file fake directories may override individual programs.
export PATH="$TESTS_DIR/fakes:$PATH"

# --- the fake server is published before every helper run (SH-840) ---------
#
# fakes/tmux keeps its server model in $FAKE_TMUX_STATE, but the native
# resource inventory reads that model from the DAEMON, through
# `tmux -S <socket> list-panes`, with an environment allowlist that carries no
# FAKE_* knob. So the model must be published from the caller's environment
# first: the socket created, the caller's FAKE_TMUX_PANES rows and worktree
# directories written down. The fake's `display-message -p '#{socket_path}'` arm
# does exactly that. Until SH-825 (90a4a55a) the helper asked that question
# itself before every inventory, so publication was a side effect of production
# code. Production now reads the caller's socket from $TMUX without asking --
# rightly: an unrelated broken caller server must not veto a lease -- and every
# fixture that leaned on the side effect read an empty server (sixteen plugin
# scripts on dev b904c137). The fixture publishes for itself now, immediately
# before each run of the helper, the one point every test passes through.
#
# Only the fake is published: the server must answer with this fixture's own
# socket, so a real tmux on PATH is left untouched. A caller outside tmux is
# pointed at the default server, so that path is linked to the fake's socket --
# but only inside the test home and never over anything but an earlier link.
# The link stays for the rest of the test, because helper runs that bypass this
# wrapper (`env ... bash "$SCRIPT"`) and daemon work that outlives a run still
# reach the default server. A test that starts a REAL server on its own default
# path declares it through _register_tmp_tmux_session, which withdraws the link
# first (test-dispatch-failure-cleanup.sh otherwise met "Socket operation on
# non-socket").
_publish_fake_tmux() {
  local answer state default_dir
  [ -n "${FAKE_TMUX_STATE:-}" ] && [ -d "$FAKE_TMUX_STATE" ] || return 0
  answer=$(tmux display-message -p '#{socket_path}' 2>/dev/null) || return 0
  state=$(cd "$FAKE_TMUX_STATE" && pwd -P) || return 1
  [ "$answer" = "$state/tmux.sock" ] || return 0
  case "${TMUX_TMPDIR:-}" in "$STORYHOOK_TEST_HOME"/*) ;; *) return 0 ;; esac
  default_dir="$TMUX_TMPDIR/tmux-$(id -u)"
  mkdir -p "$TMUX_TMPDIR" || return 1
  [ -d "$default_dir" ] || mkdir -m 700 "$default_dir" || return 1
  if [ -L "$default_dir/default" ] || [ ! -e "$default_dir/default" ]; then
    ln -sfn "$answer" "$default_dir/default" || return 1
  fi
}

# Withdraw a fake default-server link before a real server needs the path. Only
# a link to a regular file is the fake's: a real server's socket is a socket,
# and a link to one (tmux-revivify makes them) is never removed.
_withdraw_fake_tmux_default() {
  local link target
  case "${TMUX_TMPDIR:-}" in "$STORYHOOK_TEST_HOME"/*) ;; *) return 0 ;; esac
  link="$TMUX_TMPDIR/tmux-$(id -u)/default"
  [ -L "$link" ] || return 0
  target=$(readlink "$link") || return 1
  [ -f "$target" ] && [ ! -S "$target" ] || return 0
  rm -f -- "$link"
}

# Every test runs the helper as `bash "$SCRIPT" ...` (or an installed copy,
# `bash <...>/story.sh`), so this one wrapper publishes for all of them without
# a hand-kept list of call sites -- the reason the `git` wrapper above exists.
# A per-call `FAKE_TMUX_PANES=... bash "$SCRIPT"` reaches the function's
# environment, exactly as it reached the helper's own probe.
bash() {
  case "${1:-}" in
    "$SCRIPT" | */story.sh)
      _publish_fake_tmux || {
        printf 'lib.sh: cannot publish the fake tmux server in %s (SH-840)\n' "${FAKE_TMUX_STATE:-}" >&2
        return 1
      } ;;
  esac
  command bash "$@"
}

# mk_story_repo — build a temp git repo with a real storyhook project
# initialized (`story project new`), and a LOCAL bare origin so dispatch's `git
# fetch` resolves fully offline and deterministically (no network, no
# credential prompt). Echoes the repo path. Not placed under $TMPDIR:
# macOS Spotlight indexes it and can stall file-intensive tests; /tmp
# (aka /private/tmp) is not indexed.
#
# Takes an optional story-id prefix (default TST), so a test needing TWO
# projects can tell their ids apart — SH-120's dispatch-into-the-wrong-repo
# fixtures need exactly that.
mk_story_repo() {
  local origdir origin repo prefix="${1:-TST}"
  origdir="$(mktemp -d /tmp/story-test-origin.XXXXXX)"
  _register_tmp "$origdir"
  mkdir -p "$origdir/fake"
  origin="$origdir/fake/repo.git"
  git init -q --bare -b main "$origin"
  repo="$(mktemp -d /tmp/story-test.XXXXXX)"
  _register_tmp "$repo"
  (
    cd "$repo" || exit 1
    git init -q -b main
    git config user.email t@t
    git config user.name t
    git remote add origin "$origin"
    echo a >f
    # Mirrors this repo's own top-level .gitignore rule for the dispatch
    # sentinel (SH-231) — a real onboarded storyhook repo carries this, and a
    # fixture that omits it makes every dispatched worktree spuriously
    # "dirty" the instant its SessionStart hook (here, the fake tmux
    # standing in for one) publishes a sentinel, which test-bare-story-id.sh's
    # removable/dirty classification would otherwise misreport.
    printf '%s\n' '.claude/dispatch-sentinel.json' >.gitignore
    git add f .gitignore
    git commit -qm init
    git push -qu origin main
    git remote set-head origin main >/dev/null 2>&1 || true
    # `story project new` writes `.storyhook.toml` and nothing else into the tree;
    # the stories themselves go to the (isolated) store. Committing the
    # pointer is what a real project does, and it is what makes a linked
    # worktree of this fixture resolve the SAME project as the main checkout —
    # the property test-dispatch-cwd.sh asserts.
    story project new --prefix "$prefix" >/dev/null 2>&1
    git add .storyhook.toml
    git commit -qm 'storyhook pointer'
    git push -q origin main
  ) >/dev/null 2>&1
  printf '%s' "$repo"
}

# slug_for <dir> — the project slug `story project list` reports for the project
# rooted at <dir>. Read out of the listing rather than derived from the
# directory name: the derivation is the CLI's business, and a test that
# reimplemented it would keep passing after the two disagreed.
#
# Lived in test-project-selection.sh until SH-120 needed it in a second file.
slug_for() {
  local phys
  phys=$(cd "$1" && pwd -P)
  (cd "$1" && story project list 2>/dev/null) \
    | awk -v p="$phys" 'index($0, p) && $1 != "checkout" && $1 != "origin" {print $1; exit}'
}

# new_story <repo-dir> "<title>" — create a story via the real CLI in
# <repo-dir>, echo its assigned id.
new_story() {
  local repo="$1" title="$2"
  (cd "$repo" && story new "$title" --json 2>/dev/null | jq -r '.story.story.id')
}

# mk_versioned_claude <version> [extra-version...] — build a fake install whose
# `bin/claude` is a SYMLINK to `versions/<version>`, mirroring the layout Claude
# Code's native installer produces. Extra versions are created alongside but not
# linked, so a test can model an update landing mid-poll. Echoes the root.
#
# The symlink is the whole point (SH-239): tmux reports `#{pane_current_command}`
# as the basename of the RESOLVED executable, so a pane running this install is
# called `2.1.228`, not `claude`.
mk_versioned_claude() {
  local root version
  root="$(mktemp -d /tmp/story-test-claude.XXXXXX)"
  _register_tmp "$root"
  mkdir -p "$root/bin" "$root/versions"
  for version in "$@"; do
    printf '#!/bin/sh\nexit 0\n' >"$root/versions/$version"
    chmod +x "$root/versions/$version"
  done
  ln -s "$root/versions/$1" "$root/bin/claude"
  printf '%s' "$root"
}

# wname_for <repo-dir> <id> — the window/worktree/branch name story.sh
# derives for <id>: the bare id, unprefixed since SH-166. Takes <repo-dir>
# for parity with mk_dispatched below even though session.sh's own
# resolve_wname no longer is repo-sensitive.
wname_for() {
  printf '%s' "$2"
}

# mk_dispatched <repo-dir> <id> — create the worktree + branch exactly as
# `story.sh dispatch` would (the CURRENT, bare-id scheme), without needing
# tmux. Echoes the wname.
mk_dispatched() {
  local repo="$1" id="$2" w
  w="$(wname_for "$repo" "$id")"
  (cd "$repo" && git worktree add -q --no-track -b "worktree-$w" ".claude/worktrees/$w" HEAD) >/dev/null 2>&1
  printf '%s' "$w"
}

_FAILED=0
fail_test() {
  printf 'FAIL: %s\n' "$1" >&2
  _FAILED=1
}

# assert_eq <actual> <expected> <label>
assert_eq() {
  if [ "$1" != "$2" ]; then
    fail_test "$3 — expected [$2], got [$1]"
  fi
}

# assert_contains <haystack> <needle> <label>
assert_contains() {
  case "$1" in
  *"$2"*) : ;;
  *) fail_test "$3 — [$1] does not contain [$2]" ;;
  esac
}

# assert_ok <answer> <expected> <label> — assert a helper answer's top-level
# `.ok`. `.ok` says only THAT a verb refused; the answer's `reason` and
# `display` say WHY, and a gate log is the only evidence a load-dependent
# failure leaves behind. So a mismatch prints the WHOLE answer, raw, whatever it
# is -- JSON, a crash, nothing (SH-840: test-unclaim.sh answered ok:false once
# under gate load, and nothing else it said survived). Like assert_eq, it always
# returns 0, and an answer jq cannot read never trips a caller's `set -e`.
assert_ok() {
  local actual
  actual=$(jqf "$1" .ok) || :
  if [ "$actual" != "$2" ]; then
    fail_test "$3 — expected [$2], got [$actual] — answer: [$1]"
  fi
}

# residue_reasons <answer> <resource-prefix> — the reasons a reset answer gives
# for what it left in place, for each resource that starts with the prefix,
# joined with "|". Empty when the reset left no such resource (SH-886).
residue_reasons() {
  printf '%s' "$1" | jq -r --arg prefix "$2" \
    '[(.residue // [])[] | select(.resource | startswith($prefix)) | .reason] | join("|")'
}

# assert_held_out_of_dispatch <repo> <id> <label> — a reset that left residue
# the next dispatch would collide with released the story, held it with an
# awaiting reason, and so no dispatch is offered it (SH-886 decision D7). This
# is what a refused reset's kept claim protected under the old contract.
assert_held_out_of_dispatch() {
  local repo="$1" id="$2" label="$3" shown ready
  shown=$(cd "$repo" && story show "$id" --json) || :
  assert_eq "$(jqf "$shown" .story.story.state)" todo "$label: reset released the story"
  assert_contains "$(jqf "$shown" '.story.story.awaiting // ""')" "next dispatch would collide with" \
    "$label: the residue holds the story out of dispatch"
  ready=",$(cd "$repo" && story list --ready --json | jq -r '[.stories[]?.story.id] | join(",")'),"
  case "$ready" in
  *",$id,"*) fail_test "$label: the held story is still offered for dispatch" ;;
  esac
}

# router_verbs <story.sh> — derive the helper's accepted verb vocabulary from
# its top-level router. Keep every structural inventory on this one parser so
# a new arm cannot require several hand-list edits to remain covered.
router_verbs() {
  awk '/^case "\$\{1:-\}" in$/,/^esac$/' "$1" \
    | sed -n 's/^  \([a-z][a-z-]*\)).*/\1/p'
}

# jqf <json> <filter> — run a jq filter, echo the raw result.
jqf() { printf '%s' "$1" | jq -r "$2"; }

finish() {
  if [ "$_FAILED" -eq 0 ]; then
    echo "PASS"
    exit 0
  else
    exit 1
  fi
}

# Give GitHub integration fixtures a valid origin while retaining real Git data.
# GITHUB_FIXTURE_ORIGIN remains available for direct server-side assertions.
github_fixture() {
  local repo="$1" url="$2" bin
  GITHUB_FIXTURE_ORIGIN=$(git -C "$repo" remote get-url origin) || return 1
  bin="$repo/.git/github-endpoint"
  python3 "$TESTS_DIR/../../../scripts/test-git-endpoint.py" "$bin" \
    "$(jq -n --arg url "$url" --arg path "$GITHUB_FIXTURE_ORIGIN" '{($url):$path}')" || return 1
  git -C "$repo" remote set-url origin "$url" || return 1
  export PATH="$bin:$PATH"
}
