#!/usr/bin/env bash
# post-git.sh's skip-guard, and the question it must ask about the *verb*
# (SH-330).
#
# `test-post-git-hooks-path.sh` covers the other half — SH-320's question about
# *where* the hook is. This file is about which git command was run, and they
# are kept apart because they are two attributable defects: the guard was
# path-blind, and it was verb-blind.
#
# The guard exists to avoid a double sync: if git will itself run the managed
# `post-commit`, the plugin's stand-in `story commit-sync` is redundant. It
# asked that one question for all three verbs it watches, which credits
# `post-commit` for work it never ran:
#
#   * **`git push` runs no post-push hook.** Git has none and storyhook's
#     `HOOKS` table has none, so the guard skipped on the strength of a hook
#     that cannot have fired.
#   * **`git merge` runs `post-merge`, not `post-commit`** — measured. Before
#     SH-330's other half, `post-merge` never synced at all, so a merge's
#     commits were linked by nobody once the guard skipped.
#
# Stated positively, the rule the cases below pin: **skip only when every git
# verb in this command is one a managed `post-commit` covers.**
#
# Observed the way the path file observes it: a fake `story` on PATH that
# records it was reached. "Skipped" is the absence of that record, which is the
# only thing a no-op can be observed by.
#
# Every case runs against a repository that HOLDS an executable marker-bearing
# `post-commit` — the exact state that makes the guard skip. Without that the
# suite would pass vacuously: nothing would skip regardless of the verb, and a
# reverted fix would still be green.
source "$(dirname "$0")/lib.sh"

HOOKS_DIR="$PLUGIN_ROOT/hooks"
MARKER="# storyhook managed hook -- do not edit this line"

FAKE_DIR=$(mktemp -d /tmp/story-test-verbfake.XXXXXX)
_TMP_REPOS+=("$FAKE_DIR")
export STORY_FAKE_LOG="$FAKE_DIR/reached"
cat >"$FAKE_DIR/story" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STORY_FAKE_LOG"
printf 'the fake story CLI ran\n'
FAKE
chmod +x "$FAKE_DIR/story"
export PATH="$FAKE_DIR:$PATH"

# mk_repo — a git repository under /tmp with one commit. `/tmp`, never
# `$TMPDIR`: macOS Spotlight indexes the latter and stalls fixture-heavy runs
# (SH-53).
mk_repo() {
  local repo
  repo=$(mktemp -d /tmp/story-test-verbrepo.XXXXXX)
  _TMP_REPOS+=("$repo")
  git init -q "$repo"
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name "Test"
  git -C "$repo" commit -q --allow-empty -m "init"
  printf '%s' "$repo"
}

# write_managed_hook <path> — the managed post-commit as `src/hooks.rs` writes
# it: the marker line, and mode 0755, which is what makes git willing to run it.
write_managed_hook() {
  local path="$1"
  mkdir -p "$(dirname "$path")"
  printf '#!/bin/sh\n%s\ncommand -v story >/dev/null 2>&1 || exit 0\nstory commit-sync --since 1h --quiet 2>/dev/null || true\n' \
    "$MARKER" >"$path"
  chmod 755 "$path"
}

# run_verb <dir> <command> — post-git.sh in <dir>, fed a PostToolUse payload
# carrying <command>, with a fresh reach log.
run_verb() {
  : >"$STORY_FAKE_LOG"
  local payload
  payload=$(COMMAND="$2" python3 -c '
import json, os
print(json.dumps({"tool_input": {"command": os.environ["COMMAND"]}}))')
  (cd "$1" && printf '%s' "$payload" | bash "$HOOKS_DIR/post-git.sh" 2>/dev/null)
}

# reached — did the hook get as far as running `story`?
reached() { [ -s "$STORY_FAKE_LOG" ] && printf 'yes' || printf 'no'; }

# --- the control: a commit IS covered by post-commit, so it still skips ------
#
# Without this case every other one passes for the wrong reason — a guard that
# never skips at all would satisfy them. This is the behaviour SH-320 verified
# and SH-330 must preserve.
repo=$(mk_repo)
write_managed_hook "$repo/.git/hooks/post-commit"
out=$(run_verb "$repo" "git commit -m wip")
assert_eq "$(reached)" "no" "git commit runs the managed post-commit, which syncs — skip"
assert_eq "$out" "{}" "and says nothing"

# --- a merge runs post-merge, not post-commit -------------------------------
#
# Measured: `git merge --no-ff` fires post-merge ONLY. Whatever post-merge does
# with what it is handed, a post-commit that did not run is no reason to skip.
repo=$(mk_repo)
write_managed_hook "$repo/.git/hooks/post-commit"
out=$(run_verb "$repo" "git merge --no-ff side")
assert_eq "$(reached)" "yes" "a merge never runs post-commit — its presence is no reason to skip"

# --- a push runs no post-push hook, because git has none --------------------
repo=$(mk_repo)
write_managed_hook "$repo/.git/hooks/post-commit"
out=$(run_verb "$repo" "git push origin main")
assert_eq "$(reached)" "yes" "git has no post-push hook to credit — sync"

# --- a compound command falls open ------------------------------------------
#
# The case a naive "does it contain `git commit`?" rewrite gets wrong. Both
# halves ran; only one of them is covered; so the command as a whole is not.
repo=$(mk_repo)
write_managed_hook "$repo/.git/hooks/post-commit"
out=$(run_verb "$repo" "git commit -m x && git merge --no-ff side")
assert_eq "$(reached)" "yes" \
  "a command whose verbs are not ALL covered by post-commit must sync"

# --- the verb test did not swallow the fall-through -------------------------
#
# A push in a repository with no managed hook at all. It must still sync — for
# the ordinary reason, not because the verb test short-circuited something. If
# the guard were rewritten to skip on merge/push instead, this stays green while
# the cases above go red, which is what makes it worth asserting separately.
repo=$(mk_repo)
out=$(run_verb "$repo" "git push origin main")
assert_eq "$(reached)" "yes" "no managed hook anywhere — sync, as always"

finish
