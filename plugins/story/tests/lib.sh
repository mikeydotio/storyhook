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
PLUGIN_ROOT="$(cd "$TESTS_DIR/.." && pwd)"
SCRIPT="$PLUGIN_ROOT/bin/story.sh"

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
  export STORYHOOK_REAL_HOME="${STORYHOOK_REAL_HOME:-$HOME}"
  STORYHOOK_TEST_HOME="$(mktemp -d /tmp/storyhook-plugin-home.XXXXXX)"
  export STORYHOOK_TEST_HOME
  _STORYHOOK_OWNS_TEST_HOME=1
  _TMP_REPOS+=("$STORYHOOK_TEST_HOME")

  # THE ISOLATION, in one shared place -- `scripts/test-env.sh`, whose own
  # header carries the parameters and the reason for each. `--home` IS passed:
  # this suite runs nothing but `story` and `git`.
  storyhook_isolate --home "$STORYHOOK_TEST_HOME"

  # A standalone `bash test-foo.sh` (this branch) has no SH-524 progress
  # journal of its own to write to; an ambient one set by some other daemon-
  # owned run must not be inherited and mistaken for this test's. Not a
  # test-environment parameter -- a harness legitimately SETS this one -- so it
  # stays here rather than joining the shared table.
  unset STORYHOOK_GATE_PROGRESS
fi

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
_STORY_TARGET_DIR="${CARGO_TARGET_DIR:-$(cd "$TESTS_DIR/../../.." && pwd)/target}"
if [ ! -x "$_STORY_TARGET_DIR/debug/story" ]; then
  echo "refusing to run: $_STORY_TARGET_DIR/debug/story does not exist." >&2
  echo "  This suite tests the \`story\` THIS checkout builds, never the one" >&2
  echo "  installed on the machine -- an installed binary is a different" >&2
  echo "  version whose failures read as product bugs. Run \`cargo build\`" >&2
  echo "  first, or \`make test\`, which does." >&2
  exit 1
fi
# shellcheck source=../../../scripts/binary-lease.sh
. "$TESTS_DIR/../../../scripts/binary-lease.sh"
if [ "${_STORYHOOK_OWNS_TEST_HOME:-0}" = 1 ]; then
  _STORY_LEASE="$(storyhook_lease_binary "$_STORY_TARGET_DIR/debug/story")" || exit 1
  _STORY_LEASE_DIR="$(dirname "$_STORY_LEASE")"
  _TMP_REPOS+=("$_STORY_LEASE_DIR")
  export PATH="$_STORY_LEASE_DIR:$PATH"
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
  unset _STORY_INHERITED
fi
unset _STORY_TARGET_DIR

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
