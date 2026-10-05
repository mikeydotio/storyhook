#!/usr/bin/env bash
# SH-884: exercise the system submission boundary with real local Git.
source "$(dirname "$0")/lib.sh"

FAKE_GH_STATE="$(mktemp -d /tmp/story-test-gh.XXXXXX)"
export FAKE_GH_STATE
_TMP_REPOS+=("$FAKE_GH_STATE")
FAKES_PATH="$TESTS_DIR/fakes:$PATH"

repo=$(mk_story_repo SUB)
github_fixture "$repo" "https://github.com/acme/widgets.git"
FAKES_PATH="$repo/.git/github-endpoint:$FAKES_PATH"
slug=$(slug_for "$repo")
id=$(new_story "$repo" "Publish a repair on commit")
name=$(mk_dispatched "$repo" "$id")
worktree="$repo/.claude/worktrees/$name"
branch="worktree-$name"
repository_path=$(cd "$repo" && pwd -P)
worktree_path=$(cd "$worktree" && pwd -P)
origin="$GITHUB_FIXTURE_ORIGIN"
socket="$FAKE_TMUX_STATE/tmux.sock"

lease=$(jq -n --arg project "$slug" --arg story "$id" \
  --arg repository "$repository_path" --arg worktree "$worktree_path" \
  --arg branch "$branch" --arg socket "$socket" \
  '{version:1,project_slug:$project,story_id:$story,
    repository_path:$repository,worktree_path:$worktree,branch:$branch,
    tmux:{socket_path:$socket}}')

real_story=$(command -v story)
# The story is reported as `verifying` by the proxy in fakes/story-verifying
# and never actually parked there: the real daemon's verifier would pick a
# real `verifying` story up within the same second (see the proxy's header).
VERIFYING_PATH="$TESTS_DIR/fakes/story-verifying:$FAKES_PATH"

# submit — the daemon's exact invocation shape:
# cwd is the leased repository, STORY_AGENT is absent, the lease rides the
# private variable, and gh is the fake. Echoes stdout+stderr; status in $?.
submit() {
  (cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" \
    GH_PROMPT_DISABLED=1 PATH="$VERIFYING_PATH" STORY_REAL_BIN="$real_story" \
    STORY_SHOW_AS_VERIFYING="$id" \
    bash "$SCRIPT" --project "$slug" submit "$id" 2>&1)
}
create_count() { grep -c $'^pr\tcreate\t' "$FAKE_GH_STATE/argv.log" 2>/dev/null || printf '0'; }
remote_tip() { git -C "$repo" ls-remote --heads origin "$branch" | cut -f1; }


# First submission establishes the existing pull request.
printf 'initial\n' >"$worktree/work.txt"
git -C "$worktree" add work.txt
git -C "$worktree" -c user.name=t -c user.email=t@e commit -qm 'feat: initial'
out=$(submit)
assert_ok "$out" true 'initial submission'
pr=$(jqf "$out" .pull_request.url)
old=$(remote_tip)

(cd "$repo" && story move "$id" in-progress --quiet)

# The returned story is actually in-progress, with uncommitted work left.
printf 'repair\n' >>"$worktree/work.txt"
git -C "$worktree" -c user.name=t -c user.email=t@e commit -qam 'fix: repair without story reference'
head=$(git -C "$worktree" rev-parse HEAD)
printf 'unfinished\n' >"$worktree/unfinished.txt"
publish() {
  (cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" \
    STORYHOOK_PUBLICATION_HEAD="$head" STORYHOOK_PUBLICATION_PR="$pr" \
    GH_PROMPT_DISABLED=1 PATH="$VERIFYING_PATH" STORY_REAL_BIN="$real_story" STORY_SHOW_AS_VERIFYING="" \
    bash "$SCRIPT" --project "$slug" submit "$id" 2>&1)
}
out=$(publish)
assert_ok "$out" true 'in-progress repair publishes while worktree is dirty'
assert_eq "$(remote_tip)" "$head" 'repair reaches origin without verifier admission'
assert_eq "$(jqf "$out" .pull_request.head_oid)" "$head" 'receipt names exact repair'
assert_eq "$(create_count)" 1 'repair adopts existing PR'
[ -f "$worktree/unfinished.txt" ] || fail_test 'publication modified working files'
out=$(publish)
assert_ok "$out" true 'duplicate publication succeeds'
assert_eq "$(jqf "$out" .pushed)" false 'duplicate publication is idempotent'

# A competing remote commit cannot be overwritten by repair publication.
remote_tree=$(git --git-dir="$origin" rev-parse "$old^{tree}")
diverged=$(printf 'remote divergence\n' | git --git-dir="$origin" -c user.name=t -c user.email=t@e commit-tree "$remote_tree" -p "$old")
git --git-dir="$origin" update-ref "refs/heads/$branch" "$diverged" "$head"
out=$(publish)
assert_ok "$out" false 'diverged remote refuses publication'
assert_eq "$(jqf "$out" .reason)" push-rejected 'divergence has an actionable refusal'
assert_eq "$(remote_tip)" "$diverged" 'remote history was not overwritten'
git --git-dir="$origin" update-ref "refs/heads/$branch" "$head" "$diverged"

# Closed PRs must not cause a push or replacement PR.
printf 'later\n' >>"$worktree/work.txt"
git -C "$worktree" -c user.name=t -c user.email=t@e commit -qam 'fix: later'
head=$(git -C "$worktree" rev-parse HEAD)
jq 'map(.state="MERGED")' "$FAKE_GH_STATE/prs.json" >"$FAKE_GH_STATE/prs.tmp"
mv "$FAKE_GH_STATE/prs.tmp" "$FAKE_GH_STATE/prs.json"
out=$(publish)
assert_ok "$out" false 'closed PR publication refuses'
assert_eq "$(jqf "$out" .reason)" publication-pr-unavailable 'closed PR diagnostic'
assert_eq "$(create_count)" 1 'closed PR does not create replacement'
[ "$(remote_tip)" != "$head" ] || fail_test 'closed PR was pushed'

# Run the real managed hook and daemon, with only GitHub/Git transport endpoints
# replaced. Stopped verification must not stop publication from an active lane.
jq 'map(.state="OPEN")' "$FAKE_GH_STATE/prs.json" >"$FAKE_GH_STATE/prs.tmp"
mv "$FAKE_GH_STATE/prs.tmp" "$FAKE_GH_STATE/prs.json"
private=$(git -C "$worktree" rev-parse --absolute-git-dir)
printf '%s\n' "$lease" >"$private/storyhook-cleanup-lease-v1.json"
export GH_CONFIG_DIR="$FAKE_GH_STATE"
export STORYHOOK_DISPATCH_SCRIPT="$SCRIPT"
export PATH="$FAKES_PATH"
(cd "$repo" && story daemon stop --force >/dev/null)
(cd "$repo" && story verifier stop --quiet)
(cd "$repo" && story link-pr "$id" "$pr" --quiet)
(cd "$worktree" && story hooks install --quiet)
printf 'hook repair\n' >>"$worktree/work.txt"
git -C "$worktree" -c user.name=t -c user.email=t@e commit -qam 'fix: hook repair'
hook_head=$(git -C "$worktree" rev-parse HEAD)
wait_remote() {
python3 - "$repo" "$branch" "$1" <<'PY'
import subprocess, sys, time
repo, branch, head = sys.argv[1:]
deadline = time.monotonic() + 60
while time.monotonic() < deadline:
    result = subprocess.run(['git', '-C', repo, 'ls-remote', '--heads', 'origin', branch],
                            capture_output=True, text=True, timeout=10)
    if result.returncode == 0 and result.stdout.split()[:1] == [head]:
        break
    time.sleep(0.1)
else:
    raise SystemExit('managed post-commit did not publish the repair')
PY
}
wait_remote "$hook_head"
[ "$?" -eq 0 ] || fail_test "daemon publication failed: $(cd "$repo" && story show "$id" --json)"
assert_eq "$(remote_tip)" "$hook_head" 'managed hook publishes with verification stopped'
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r .story.story.state)" in-progress 'publication does not submit the story'

# Persist a failed request, restart the daemon, and let its normal retry recover.
printf '\nexport FAKE_GH_FAIL=publication-fixture-offline\n' >>"$FAKE_GH_STATE/fixture-env"
printf 'restart repair\n' >>"$worktree/work.txt"
git -C "$worktree" -c user.name=t -c user.email=t@e commit -qam 'fix: repair retained through restart'
printf 'second queued repair\n' >>"$worktree/work.txt"
git -C "$worktree" -c user.name=t -c user.email=t@e commit -qam 'fix: coalesced repair'
hook_head=$(git -C "$worktree" rev-parse HEAD)
python3 - "$repo" "$id" <<'PY'
import json, subprocess, sys, time
repo, story = sys.argv[1:]
deadline = time.monotonic() + 60
while time.monotonic() < deadline:
    result = subprocess.run(['story', 'show', story, '--json'], cwd=repo,
                            capture_output=True, text=True, timeout=15)
    if result.returncode == 0:
        comments = json.loads(result.stdout)['story']['story']['comments']
        if any('REPAIR PUBLICATION FAILED' in c['text'] for c in comments):
            break
    time.sleep(0.2)
else:
    raise SystemExit('publication failure was not reported')
PY
[ "$?" -eq 0 ] || fail_test 'publication failure must be visible before restart'
[ "$(remote_tip)" != "$hook_head" ] || fail_test 'offline endpoint unexpectedly published'
(cd "$repo" && story daemon stop --force >/dev/null)
printf '\nunset FAKE_GH_FAIL\n' >>"$FAKE_GH_STATE/fixture-env"
(cd "$worktree" && story commit-sync --quiet)
wait_remote "$hook_head" || fail_test 'retained publication did not recover after daemon restart'
assert_eq "$(remote_tip)" "$hook_head" 'restart publishes the retained repair'

# A queued returned story is then overridden. Model only the external merge:
# the actual remote PR head is merged, never a SHA supplied by the test.
(cd "$worktree" && story move "$id" verifying --quiet)
(cd "$repo" && story move "$id" "done" 'fixture override' --quiet)
published=$(remote_tip)
base=$(git --git-dir="$origin" rev-parse refs/heads/main)
tree=$(git --git-dir="$origin" merge-tree --write-tree "$base" "$published")
merged=$(printf 'fixture PR merge\n' | git --git-dir="$origin" -c user.name=t -c user.email=t@e commit-tree "$tree" -p "$base" -p "$published")
git --git-dir="$origin" update-ref refs/heads/main "$merged" "$base"
git --git-dir="$origin" merge-base --is-ancestor "$hook_head" "$merged" || fail_test 'override merged head omits repair'
assert_eq "$(create_count)" 1 'daemon publication creates no replacement PR'
