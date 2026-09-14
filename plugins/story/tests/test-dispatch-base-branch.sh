#!/usr/bin/env bash
# SH-691: dispatch bases a new worktree on origin's ADVERTISED default branch,
# asked of the remote — never on the local origin/HEAD cache alone, and never
# on a literal. When origin does not answer, the receipt says which fallback
# was used (`base_source`: origin, cache, none) and the warning names the
# remedy. The old `main` fallback is how five stories reached the wrong
# branch, so the stale-cache case here is the one a restored literal fails.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

# dispatch_real <repo> <id> [VAR=value...] — the happy-path harness shape,
# with optional extra environment for the knob cases.
dispatch_real() {
  local dir="$1" id="$2"; shift 2
  (
    cd "$dir" \
      && env PATH="$FAKE_TMUX_DIR:$PATH" \
        TMUX="fake,0,0" TMUX_PANE="%0" \
        STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
        STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
        FAKE_TMUX_CAPTURE=marker "$@" \
        bash "$SCRIPT" dispatch "$id" 2>&1
  )
}

repo=$(mk_story_repo)
origin=$(git -C "$repo" remote get-url origin)

# --- origin's default moved to dev; the local cache still says main ---------
(cd "$repo" \
  && git checkout -q -b dev \
  && printf 'dev\n' >dev.txt && git add dev.txt && git commit -qm 'dev work' \
  && git push -q origin dev \
  && git checkout -q main)
git --git-dir="$origin" symbolic-ref HEAD refs/heads/dev
dev_tip=$(git --git-dir="$origin" rev-parse refs/heads/dev)
assert_eq "$(git -C "$repo" symbolic-ref refs/remotes/origin/HEAD)" "refs/remotes/origin/main" \
  "fixture: the local origin/HEAD cache still says main"
id=$(new_story "$repo" "Based on the real default")
out=$(dispatch_real "$repo" "$id")
assert_eq "$(jqf "$out" .ok)" "true" "stale cache: dispatch succeeds: $out"
assert_eq "$(jqf "$out" .base_branch)" "dev" "stale cache: the base is origin's advertised default"
assert_eq "$(jqf "$out" .base_source)" "origin" "stale cache: …asked of origin"
assert_eq "$(jqf "$out" .base_ref)" "origin/dev" "stale cache: base_ref names it"
assert_eq "$(jqf "$out" .base_oid)" "$dev_tip" "stale cache: the worktree is based on dev's tip"
assert_eq "$(jqf "$out" .base_fresh)" "true" "stale cache: the base was freshly fetched"
assert_eq "$(jqf "$out" 'has("warning")')" "false" "stale cache: nothing to warn about"
assert_eq "$(git -C "$repo/.claude/worktrees/$id" rev-parse HEAD)" "$dev_tip" \
  "stale cache: the worktree HEAD is dev's tip"

# --- origin unreachable: the cache is used, and says so --------------------
git -C "$repo" remote set-url origin /nonexistent/storyhook-origin.git
main_tip=$(git -C "$repo" rev-parse refs/remotes/origin/main)
id2=$(new_story "$repo" "Offline with a cache")
out=$(dispatch_real "$repo" "$id2")
assert_eq "$(jqf "$out" .ok)" "true" "cache: dispatch still succeeds offline: $out"
assert_eq "$(jqf "$out" .base_branch)" "main" "cache: the cached default is used"
assert_eq "$(jqf "$out" .base_source)" "cache" "cache: …and reported as the cache"
assert_eq "$(jqf "$out" .base_oid)" "$main_tip" "cache: based on the last-known origin/main"
assert_eq "$(jqf "$out" .base_fresh)" "false" "cache: not fresh"
assert_contains "$(jqf "$out" .warning)" "origin/HEAD cache" "cache: the warning names the cache"
assert_contains "$(jqf "$out" .warning)" "git remote set-head origin -a" "cache: the warning names the remedy"
assert_contains "$(jqf "$out" .warning)" "did not answer" "cache: the warning carries git's reason"
git -C "$repo" remote set-url origin "$origin"

# --- no origin at all (the e2e seed's shape): local checkout, said so ------
repo2=$(mk_story_repo TWO)
git -C "$repo2" remote remove origin
head2=$(git -C "$repo2" rev-parse HEAD)
id3=$(new_story "$repo2" "No origin")
out=$(dispatch_real "$repo2" "$id3")
assert_eq "$(jqf "$out" .ok)" "true" "none: dispatch succeeds with no origin: $out"
assert_eq "$(jqf "$out" .base_branch)" "null" "none: no default branch is claimed"
assert_eq "$(jqf "$out" .base_ref)" "null" "none: no base ref is claimed"
assert_eq "$(jqf "$out" .base_source)" "none" "none: the source is stated"
assert_eq "$(jqf "$out" .base_oid)" "$head2" "none: based on the local checkout"
assert_contains "$(jqf "$out" .warning)" "no default branch could be established" "none: the warning says so"
assert_contains "$(jqf "$out" .warning)" "local checkout" "none: …and names the fallback"

# --- STORY_REQUIRE_FRESH_BASE refuses the checkout fallback; claim rolled back ---
id4=$(new_story "$repo2" "Strict")
before_state=$(cd "$repo2" && story show "$id4" --json | jq -r '.story.story.state')
out=$(dispatch_real "$repo2" "$id4" STORY_REQUIRE_FRESH_BASE=1)
assert_eq "$(jqf "$out" .ok)" "false" "strict: refused: $out"
assert_contains "$(jqf "$out" .display)" "STORY_REQUIRE_FRESH_BASE" "strict: names the knob"
assert_eq "$(cd "$repo2" && story show "$id4" --json | jq -r '.story.story.state')" "$before_state" \
  "strict: the claim was rolled back"

finish
