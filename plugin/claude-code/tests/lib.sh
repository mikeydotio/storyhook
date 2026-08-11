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

# Defensive: dispatch runs a real `git fetch`. If a fixture regression ever
# points it at a real origin, this stops it from blocking on a credential
# prompt -- it fails fast instead.
export GIT_TERMINAL_PROMPT=0

_TMP_REPOS=()
_cleanup() { local d; for d in "${_TMP_REPOS[@]:-}"; do [ -n "$d" ] && rm -rf "$d"; done; }
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
# run-tests.sh exports these for the whole run and refuses to start if they
# are wrong; this block is what makes a single `bash test-foo.sh` just as
# safe. STORYHOOK_REAL_HOME survives so a test can assert the real data home
# was left alone.
if [ -z "${STORYHOOK_TEST_HOME:-}" ]; then
  export STORYHOOK_REAL_HOME="$HOME"
  STORYHOOK_TEST_HOME="$(mktemp -d /tmp/storyhook-plugin-home.XXXXXX)"
  export STORYHOOK_TEST_HOME
  _TMP_REPOS+=("$STORYHOOK_TEST_HOME")
  export HOME="$STORYHOOK_TEST_HOME/home"
  export XDG_DATA_HOME="$HOME/.local/share"
  export XDG_CONFIG_HOME="$HOME/.config"
  export XDG_STATE_HOME="$HOME/.local/state"
  export STORYHOOK_DATA_DIR="$HOME/.local/share/storyhook"
  # Outranks STORYHOOK_DATA_DIR (SH-113): an exported one in the developer's
  # shell would point this whole suite at their own store.
  unset STORYHOOK_STORE_PATH
  mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$STORYHOOK_DATA_DIR"

  # Every `story` this suite runs starts a daemon, because since SH-114 every
  # `story` does. These two are what stop those daemons poisoning anything: one
  # was found alive after a gate run, from this worktree's binary, having tried
  # for the port a developer's own dashboard uses. A daemon started here can
  # never take port 3456, and cannot outlive this run.
  export STORYHOOK_DAEMON_ADDR="${STORYHOOK_DAEMON_ADDR:-127.0.0.1:0}"
  export STORYHOOK_PARENT_PID="$$"
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
  mkdir -p "$origdir/fake"
  origin="$origdir/fake/repo.git"
  git init -q --bare -b main "$origin"
  repo="$(mktemp -d /tmp/story-test.XXXXXX)"
  _TMP_REPOS+=("$repo" "$origdir")
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

# wname_for <repo-dir> <id> — the window/worktree/branch name story.sh
# derives for <id>: the bare id, unprefixed since SH-166. Takes <repo-dir>
# for parity with legacy_wname_for below (and so callers don't need to know
# which of the two is prefix-sensitive) even though session.sh's own
# resolve_wname no longer does.
wname_for() {
  printf '%s' "$2"
}

# legacy_wname_for <repo-dir> <id> — the PRE-SH-166 name: "<repo-prefix>-<id>".
# Mirrors session.sh's repo_prefix + legacy_wname rather than hard-coding the
# value, so a change to either is caught here instead of silently
# desynchronising every legacy-adoption fixture.
legacy_wname_for() {
  local base prefix
  base="$(basename "$1")"
  prefix="$(printf '%s' "$base" | tr -cd '[:alnum:]' | cut -c1-3 | tr '[:upper:]' '[:lower:]')"
  printf '%s-%s' "$prefix" "$2"
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

# mk_dispatched_legacy <repo-dir> <id> — create the worktree + branch under
# the PRE-SH-166 prefixed scheme, simulating a story dispatched by an old
# binary. Echoes the legacy wname.
mk_dispatched_legacy() {
  local repo="$1" id="$2" w
  w="$(legacy_wname_for "$repo" "$id")"
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
