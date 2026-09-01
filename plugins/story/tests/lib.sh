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

  # THE ISOLATION, in one shared place -- `scripts/test-env.sh`, whose own
  # header carries the parameters and the reason for each. `--home` IS passed:
  # this suite runs nothing but `story` and `git`.
  #
  # Sourcing one implementation is what finally ends the duplication
  # `run-tests.sh` used to document as deliberate. It was deliberate for a real
  # reason -- this branch is SKIPPED when run-tests.sh has already set
  # $STORYHOOK_TEST_HOME, so a block written only here left the whole-suite run
  # with no isolation at all, which is how the leaked daemons were found -- and
  # that reason is answered by both call sites calling the same function rather
  # than by both carrying the same twenty lines.
  # shellcheck source=../../../scripts/test-env.sh
  . "$TESTS_DIR/../../../scripts/test-env.sh"
  storyhook_isolate --home "$STORYHOOK_TEST_HOME"

  # A standalone `bash test-foo.sh` (this branch) has no SH-524 progress
  # journal of its own to write to; an ambient one set by some other daemon-
  # owned run must not be inherited and mistaken for this test's. Not a
  # test-environment parameter -- a harness legitimately SETS this one -- so it
  # stays here rather than joining the shared table.
  unset STORYHOOK_GATE_PROGRESS
fi

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
  _TMP_REPOS+=("$root")
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
