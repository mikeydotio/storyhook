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

# mk_story_repo — build a temp git repo with a real storyhook project
# initialized (`story init`), and a LOCAL bare origin so dispatch's `git
# fetch` resolves fully offline and deterministically (no network, no
# credential prompt). Echoes the repo path. Not placed under $TMPDIR:
# macOS Spotlight indexes it and can stall file-intensive tests; /tmp
# (aka /private/tmp) is not indexed.
mk_story_repo() {
  local origdir origin repo
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
    git add f
    git commit -qm init
    git push -qu origin main
    git remote set-head origin main >/dev/null 2>&1 || true
    story init --prefix TST >/dev/null 2>&1
    git add .storyhook
    git commit -qm "chore: init storyhook" >/dev/null 2>&1 || true
    git push -q origin main >/dev/null 2>&1 || true
  ) >/dev/null 2>&1
  printf '%s' "$repo"
}

# new_story <repo-dir> "<title>" — create a story via the real CLI in
# <repo-dir>, echo its assigned id.
new_story() {
  local repo="$1" title="$2"
  (cd "$repo" && story new "$title" --json 2>/dev/null | jq -r '.story.story.id')
}

# wname_for <repo-dir> <id> — the window/worktree/branch name story.sh
# derives for <id>. Mirrors session.sh's repo_prefix + resolve_wname rather
# than hard-coding the value, so a change to either is caught here instead of
# silently desynchronising every complete/capture fixture.
wname_for() {
  local base prefix
  base="$(basename "$1")"
  prefix="$(printf '%s' "$base" | tr -cd '[:alnum:]' | cut -c1-3 | tr '[:upper:]' '[:lower:]')"
  printf '%s-%s' "$prefix" "$2"
}

# mk_dispatched <repo-dir> <id> — create the worktree + branch exactly as
# `story.sh dispatch` would, without needing tmux. Echoes the wname.
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
