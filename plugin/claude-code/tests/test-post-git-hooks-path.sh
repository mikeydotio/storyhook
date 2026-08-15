#!/usr/bin/env bash
# post-git.sh's skip-guard, and the two questions it must ask git rather than
# assume (SH-320).
#
# The guard exists to avoid a double sync: if git will itself run the managed
# `post-commit` hook, the plugin's stand-in `story commit-sync` is redundant. It
# used to answer that by testing `.git/hooks/post-commit` — the `<root>/.git/hooks`
# assumption SH-313 and SH-314 removed from `src/`, which survived here in shell.
#
# It is wrong in both directions, and the second direction is the one SH-320's
# own description missed:
#
#   * **Fails open** — a linked worktree's `.git` is a FILE, so the test can
#     never resolve and the plugin syncs on top of a hook that already ran.
#   * **Fails closed** — with `core.hooksPath` set, a marker left in
#     `.git/hooks/post-commit` by an earlier install makes the guard skip while
#     git runs something else entirely. If that something else does not delegate,
#     nothing ever syncs. This repository is one missing chainer away from it.
#
# Observed the way `test-hook-kill-switch.sh` observes: a fake `story` on PATH
# that records it was reached. "Skipped" is the absence of that record, which is
# the only thing a no-op can be observed by.
#
# Every fixture asserts WHERE the marker is before it runs the hook. That check
# is not ceremony: a stale marker left at the old default location makes a
# `core.hooksPath` case pass for the wrong reason, putting the fixture under test
# instead of the code.
source "$(dirname "$0")/lib.sh"

HOOKS_DIR="$PLUGIN_ROOT/hooks"
MARKER="# storyhook managed hook -- do not edit this line"

FAKE_DIR=$(mktemp -d /tmp/story-test-guardfake.XXXXXX)
_TMP_REPOS+=("$FAKE_DIR")
export STORY_FAKE_LOG="$FAKE_DIR/reached"
cat >"$FAKE_DIR/story" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STORY_FAKE_LOG"
printf 'the fake story CLI ran\n'
FAKE
chmod +x "$FAKE_DIR/story"
export PATH="$FAKE_DIR:$PATH"

# The stdin payload post-git.sh reacts to. Everything else it ignores.
GIT_EVENT='{"tool_input":{"command":"git commit -m wip"}}'

# mk_repo — a git repository under /tmp with one commit. `/tmp`, never
# `$TMPDIR`: macOS Spotlight indexes the latter and stalls fixture-heavy runs
# (SH-53).
mk_repo() {
  local repo
  repo=$(mktemp -d /tmp/story-test-guardrepo.XXXXXX)
  _TMP_REPOS+=("$repo")
  git init -q "$repo"
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name "Test"
  git -C "$repo" commit -q --allow-empty -m "init"
  printf '%s' "$repo"
}

# write_managed_hook <path> [mode] — the managed post-commit hook as
# `src/hooks.rs` writes it: the marker line, and mode 0755 unless told otherwise.
write_managed_hook() {
  local path="$1" mode="${2:-755}"
  mkdir -p "$(dirname "$path")"
  printf '#!/bin/sh\n%s\ncommand -v story >/dev/null 2>&1 || exit 0\nstory commit-sync --since 1h --quiet 2>/dev/null || true\n' \
    "$MARKER" >"$path"
  chmod "$mode" "$path"
}

# assert_markers_at <repo> <label> [expected...] — every marker-bearing
# post-commit under <repo>, as paths relative to it, compared against the exact
# set the fixture meant to create.
assert_markers_at() {
  local repo="$1" label="$2"
  shift 2
  local found expected
  found=$(find "$repo" -name post-commit -type f -exec grep -l "$MARKER" {} + 2>/dev/null |
    sed "s|^$repo/||" | LC_ALL=C sort | tr '\n' ' ')
  expected=$(printf '%s\n' "$@" | LC_ALL=C sort | tr '\n' ' ')
  [ -z "$*" ] && expected=""
  assert_eq "$found" "$expected" "$label — marker sites"
}

# run_hook <dir> — post-git.sh in <dir> with a fresh reach log; echoes stdout.
run_hook() {
  : >"$STORY_FAKE_LOG"
  (cd "$1" && printf '%s' "$GIT_EVENT" | bash "$HOOKS_DIR/post-git.sh" 2>/dev/null)
}

# reached — did the hook get as far as running `story`?
reached() { [ -s "$STORY_FAKE_LOG" ] && printf 'yes' || printf 'no'; }

# --- fails closed: a stale managed marker masks a genuinely dark hook -------
#
# The sharpest case, and the one SH-320's "Extent" section says cannot happen.
# `core.hooksPath` names a directory holding no hook at all, so git runs nothing
# on commit — while a marker left in `.git/hooks/post-commit` by an earlier
# install convinces the old guard the sync is covered. Nothing syncs, silently.
repo=$(mk_repo)
mkdir -p "$repo/empty-hooks"
git -C "$repo" config core.hooksPath empty-hooks
write_managed_hook "$repo/.git/hooks/post-commit"
assert_markers_at "$repo" "dark hooksPath" ".git/hooks/post-commit"
out=$(run_hook "$repo")
assert_eq "$(reached)" "yes" \
  "a hooksPath git runs nothing from is not a reason to skip — the managed copy is dark"

# --- the hooksPath directory holds the managed hook: skip ------------------
#
# The other side of the same question. Nothing at the old default location, so a
# guard that still reads `.git/hooks/post-commit` syncs redundantly.
repo=$(mk_repo)
git -C "$repo" config core.hooksPath custom-hooks
write_managed_hook "$repo/custom-hooks/post-commit"
assert_markers_at "$repo" "hooksPath with the hook" "custom-hooks/post-commit"
out=$(run_hook "$repo")
assert_eq "$(reached)" "no" "git runs the managed hook from core.hooksPath — skip"
assert_eq "$out" "{}" "and says nothing"

# --- a linked worktree, where `.git` is a file ------------------------------
#
# SH-314's half. Hooks resolve from the COMMON directory for every worktree, so
# the hook installed once in the main checkout is the one git runs here too.
repo=$(mk_repo)
write_managed_hook "$repo/.git/hooks/post-commit"
git -C "$repo" worktree add -q "$repo/../$(basename "$repo")-wt" -b wt 2>/dev/null
worktree="$repo/../$(basename "$repo")-wt"
_TMP_REPOS+=("$worktree")
assert_eq "$([ -f "$worktree/.git" ] && printf 'file' || printf 'dir')" "file" \
  "the fixture is a real linked worktree — .git is a file"
out=$(run_hook "$worktree")
assert_eq "$(reached)" "no" "a linked worktree resolves the common hooks directory — skip"

# --- fired from a subdirectory ---------------------------------------------
#
# Pins the contract the guard depends on: `--git-path hooks` answers relative to
# the cwd it is asked from (`../../.git/hooks` here), and this script never cds,
# so the answer is used as given.
repo=$(mk_repo)
write_managed_hook "$repo/.git/hooks/post-commit"
mkdir -p "$repo/deep/nested"
out=$(run_hook "$repo/deep/nested")
assert_eq "$(reached)" "no" "the guard resolves from a subdirectory — skip"

# --- a hook git will not run is not a reason to skip ------------------------
#
# Git runs only an EXECUTABLE hook, and stores that bit in the index — so a
# tracked hooksPath directory restored without mode 100755 (an archive extract,
# a `cp` without `-p`, a regenerating tool) leaves a marker-bearing file git
# skips. A guard testing `-f` reports it as installed; `-x` asks git's own
# question. SH-198's lesson: a check that claims to be about executability
# performs one.
repo=$(mk_repo)
write_managed_hook "$repo/.git/hooks/post-commit" 644
assert_markers_at "$repo" "non-executable hook" ".git/hooks/post-commit"
out=$(run_hook "$repo")
assert_eq "$(reached)" "yes" "a non-executable managed hook is one git skips — sync"

# --- core.hooksPath naming a non-directory ---------------------------------
#
# `/dev/null` is the documented way to switch hooks off wholesale. Git runs no
# hooks at all, so the plugin's sync is the only one there will be.
repo=$(mk_repo)
git -C "$repo" config core.hooksPath /dev/null
write_managed_hook "$repo/.git/hooks/post-commit"
out=$(run_hook "$repo")
assert_eq "$(reached)" "yes" "core.hooksPath=/dev/null runs no hooks — sync"

# --- outside a git repository ----------------------------------------------
#
# `git rev-parse` exits 128 writing only to stderr. `set -euo pipefail` is live
# in this script, so the guard must not abort on it: the hook still runs, and a
# directory that is no project is refused by `story` itself (SH-119).
bare=$(mktemp -d /tmp/story-test-guardbare.XXXXXX)
_TMP_REPOS+=("$bare")
out=$(run_hook "$bare")
assert_eq "$(reached)" "yes" "outside a repository the guard falls through rather than aborting"

finish
