#!/usr/bin/env bash
# The verifier's reap accepts exactly the state the verifier writes (SH-652).
#
# The story lands in the required `done`; a project whose catalog lists
# other CLOSED states ahead of it — `shipped` first by position, `abandoned`
# first by name — must still reap that story, and must refuse one that a
# person moved into `shipped`, because that is not what the verifier wrote.
# Before SH-652 `reap` accepted only the FIRST CLOSED state, so every green
# story in such a project failed cleanup on every retry, forever.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo RLC)
slug=$(slug_for "$repo")
(cd "$repo" \
  && story state add shipped --super CLOSED >/dev/null \
  && story state add abandoned --super CLOSED >/dev/null \
  && story state reorder todo,in-progress,verifying,blocked,shipped,abandoned,done,dropped >/dev/null)
repository_path=$(cd "$repo" && pwd -P)
socket="$FAKE_TMUX_STATE/tmux.sock"
touch "$socket"
: >"$FAKE_TMUX_STATE/windows"

lease_for() { # lease_for <id> <worktree-name>
  jq -n --arg project "$slug" --arg story "$1" \
    --arg repository "$repository_path" --arg worktree "$(cd "$repo/.claude/worktrees/$2" && pwd -P)" \
    --arg branch "worktree-$2" --arg socket "$socket" \
    '{version:1,project_slug:$project,story_id:$story,
      repository_path:$repository,worktree_path:$worktree,branch:$branch,
      tmux:{socket_path:$socket}}'
}

# --- the verifier's own write: `done`, with `shipped` sorting first ---------
id=$(new_story "$repo" "Green under a straddle")
name=$(mk_dispatched "$repo" "$id")
worktree="$repo/.claude/worktrees/$name"
lease=$(lease_for "$id" "$name")
(cd "$repo" && story move "$id" done >/dev/null)

out=$(cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" \
  PATH="$TESTS_DIR/fakes:$PATH" bash "$SCRIPT" --project "$slug" reap "$id" 2>&1)
status=$?
assert_eq "$status" "0" "done under a shipped-first catalog: exits 0"
assert_eq "$(jqf "$out" .ok)" "true" "done under a shipped-first catalog: ok:true"
for post in worktree_registration_absent worktree_path_absent branch_absent tmux_story_windows_absent; do
  assert_eq "$(jqf "$out" ".postconditions.$post")" "true" "done under a shipped-first catalog: $post"
done
[ ! -e "$worktree" ] || fail_test "leased reap left the worktree of a done story"

# --- a story a person moved into the positionally first CLOSED state -------
other=$(new_story "$repo" "Shipped by hand")
oname=$(mk_dispatched "$repo" "$other")
olease=$(lease_for "$other" "$oname")
(cd "$repo" && story move "$other" shipped >/dev/null)
out=$(cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$olease" \
  PATH="$TESTS_DIR/fakes:$PATH" bash "$SCRIPT" --project "$slug" reap "$other" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "shipped is not completion: ok:false"
assert_eq "$(jqf "$out" .reason)" "not-completion-state" "shipped is not completion: reason"
assert_contains "$(jqf "$out" .display)" '`done`' "shipped is not completion: names the completion state"
[ -d "$repo/.claude/worktrees/$oname" ] || fail_test "a refused reap removed the worktree anyway"

# --- the retired knob is refused by name, never silently ignored -----------
out=$(cd "$repo" && STORY_DONE_STATE=shipped env STORYHOOK_REAP_LEASE_V1="$lease" \
  PATH="$TESTS_DIR/fakes:$PATH" bash "$SCRIPT" --project "$slug" reap "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "STORY_DONE_STATE: refused"
assert_eq "$(jqf "$out" .reason)" "story-done-state-retired" "STORY_DONE_STATE: named reason"
assert_contains "$(jqf "$out" .display)" "STORY_DONE_STATE" "STORY_DONE_STATE: names the knob"
assert_contains "$(jqf "$out" .display)" "Unset STORY_DONE_STATE" "STORY_DONE_STATE: names the remedy"
# An empty value is still the request (presence, not content — SH-534).
out=$(cd "$repo" && STORY_DONE_STATE= bash "$SCRIPT" --project "$slug" list 2>&1)
assert_eq "$(jqf "$out" .reason)" "story-done-state-retired" "STORY_DONE_STATE: empty value still refused"

# --- SH-691: a reap that cannot establish origin's default refuses by name ---
# Deleting on a guessed base is the one thing a reap must never do: an origin
# whose HEAD is detached advertises no default, and the old helper answered
# `main` for exactly that.
origin=$(git -C "$repo" remote get-url origin)
git --git-dir="$origin" update-ref --no-deref HEAD "$(git --git-dir="$origin" rev-parse refs/heads/main)"
unk=$(new_story "$repo" "Unknown default")
unkname=$(mk_dispatched "$repo" "$unk")
unklease=$(lease_for "$unk" "$unkname")
(cd "$repo" && story move "$unk" done >/dev/null)
out=$(cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$unklease" \
  PATH="$TESTS_DIR/fakes:$PATH" bash "$SCRIPT" --project "$slug" reap "$unk" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unknown default: ok:false"
assert_eq "$(jqf "$out" .reason)" "default-branch-unknown" "unknown default: reason"
assert_contains "$(jqf "$out" .display)" "no symbolic HEAD" "unknown default: says what origin advertised"
[ -d "$repo/.claude/worktrees/$unkname" ] || fail_test "unknown default: a refused reap removed the worktree"
(cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$unkname") \
  || fail_test "unknown default: a refused reap deleted the branch"
git --git-dir="$origin" symbolic-ref HEAD refs/heads/main

finish
