#!/usr/bin/env bash
# `story.sh submit` is the verifier's, never the agent's (SH-647): from the
# dispatch lease it pushes the leased branch to origin, then opens the pull
# request against the default branch or adopts the one already open for that
# head, and answers with a receipt the daemon checks field for field. The
# remote is a REAL bare repository (mk_story_repo's origin), so "pushed" is
# proven by reading the remote ref back, never by trusting git's exit code;
# only `gh` is a fake, and it records every argv so adopt-versus-create is a
# measured count, not an inference.
source "$(dirname "$0")/lib.sh"

FAKE_GH_STATE="$(mktemp -d /tmp/story-test-gh.XXXXXX)"
export FAKE_GH_STATE
_TMP_REPOS+=("$FAKE_GH_STATE")
FAKES_PATH="$TESTS_DIR/fakes:$PATH"

repo=$(mk_story_repo SUB)
slug=$(slug_for "$repo")
id=$(new_story "$repo" "Submit from the lease")
name=$(mk_dispatched "$repo" "$id")
worktree="$repo/.claude/worktrees/$name"
branch="worktree-$name"
repository_path=$(cd "$repo" && pwd -P)
worktree_path=$(cd "$worktree" && pwd -P)
origin=$(git -C "$repo" remote get-url origin)
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

# submit [extra env assignments...] — the daemon's exact invocation shape:
# cwd is the leased repository, STORY_AGENT is absent, the lease rides the
# private variable, and gh is the fake. Echoes stdout+stderr; status in $?.
submit() {
  (cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" \
    GH_PROMPT_DISABLED=1 PATH="$VERIFYING_PATH" STORY_REAL_BIN="$real_story" \
    STORY_SHOW_AS_VERIFYING="$id" "$@" \
    bash "$SCRIPT" --project "$slug" submit "$id" 2>&1)
}
create_count() { grep -c $'^pr\tcreate\t' "$FAKE_GH_STATE/argv.log" 2>/dev/null || printf '0'; }
remote_tip() { git -C "$repo" ls-remote --heads origin "$branch" | cut -f1; }

# --- usage and the lease gate ------------------------------------------------
out=$(bash "$SCRIPT" --project "$slug" submit 2>&1); status=$?
assert_eq "$status" "1" "submit without an id exits non-zero"
assert_contains "$(jqf "$out" .display)" "usage: story.sh submit <story-id>" "submit without an id prints usage"
out=$(bash "$SCRIPT" --project "$slug" submit "$id" extra 2>&1)
assert_contains "$(jqf "$out" .display)" "usage: story.sh submit <story-id>" "submit refuses a trailing argument"
out=$(cd "$repo" && env -u STORY_AGENT -u STORYHOOK_REAP_LEASE_V1 PATH="$FAKES_PATH" \
  bash "$SCRIPT" --project "$slug" submit "$id" 2>&1); status=$?
assert_eq "$status" "1" "submit without a lease exits non-zero"
assert_eq "$(jqf "$out" .reason)" "submit-requires-lease" "submit without a lease is refused by name"
assert_contains "$(jqf "$out" .display)" "story move $id verifying" "the refusal tells an agent what its own last action is"

# --- the story must be in verifying --------------------------------------------
out=$(cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" PATH="$FAKES_PATH" \
  bash "$SCRIPT" --project "$slug" submit "$id" 2>&1); status=$?
assert_eq "$status" "1" "a story not in verifying is refused"
assert_eq "$(jqf "$out" .reason)" "not-verifying" "…by name"
assert_eq "$(jqf "$out" .class)" "repair" "…as the agent's to repair"
[ -z "$(remote_tip)" ] || fail_test "a refused submit must not push"

# --- a dirty worktree is refused naming the files ----------------------------------
printf 'scratch\n' >"$worktree/untracked.txt"
printf 'edit\n' >>"$worktree/.storyhook.toml"
out=$(submit); status=$?
assert_eq "$status" "1" "a dirty worktree is refused"
assert_eq "$(jqf "$out" .reason)" "dirty-worktree" "…by name"
assert_eq "$(jqf "$out" .class)" "repair" "…as the agent's to repair"
assert_eq "$(jqf "$out" '.dirty_files | sort | join(",")')" ".storyhook.toml,untracked.txt" \
  "the receipt names every dirty file"
assert_contains "$(jqf "$out" .display)" "untracked.txt" "the display names the dirty files"
[ -z "$(remote_tip)" ] || fail_test "a dirty worktree must not be pushed"
rm -f "$worktree/untracked.txt"
git -C "$worktree" checkout -q -- .storyhook.toml

# --- nothing to submit: the tip is already on origin/<default> ---------------------
out=$(submit); status=$?
assert_eq "$status" "1" "a branch with no commits past the base is refused"
assert_eq "$(jqf "$out" .reason)" "nothing-to-submit" "…by name"
assert_eq "$(jqf "$out" .class)" "repair" "…as the agent's to repair"
assert_eq "$(create_count)" "0" "nothing-to-submit opens no pull request"

# --- the create path: push for real, open the PR, receipt ---------------------------
printf 'work\n' >"$worktree/work.txt"
git -C "$worktree" add work.txt
git -C "$worktree" -c user.name=t -c user.email=t@e commit -q -m "feat: the work"
head=$(git -C "$worktree" rev-parse HEAD)
out=$(submit); status=$?
assert_eq "$status" "0" "submit succeeds: $out"
assert_eq "$(jqf "$out" .ok)" "true" "the receipt is ok"
assert_eq "$(jqf "$out" .receipt_version)" "1" "the receipt carries the lease version"
assert_eq "$(jqf "$out" .story_id)" "$id" "the receipt echoes the story"
assert_eq "$(printf '%s' "$out" | jq -r --argjson l "$lease" '.lease == $l')" "true" "the receipt echoes the exact lease"
assert_eq "$(jqf "$out" .pushed)" "true" "the first submission pushed"
assert_eq "$(remote_tip)" "$head" "origin/$branch is the worktree HEAD after the push"
assert_eq "$(jqf "$out" .pull_request.adopted)" "false" "the pull request was opened, not adopted"
assert_eq "$(jqf "$out" .pull_request.url)" "https://github.com/acme/widgets/pull/100" "the receipt carries the URL"
assert_eq "$(jqf "$out" .pull_request.number)" "100" "the receipt carries the number"
assert_eq "$(jqf "$out" .pull_request.base)" "main" "the pull request targets the repository's default branch"
assert_eq "$(jqf "$out" .pull_request.head_oid)" "$head" "the receipt carries the head GitHub reports"
assert_eq "$(create_count)" "1" "exactly one pull request was created"
create_line=$(grep $'^pr\tcreate\t' "$FAKE_GH_STATE/argv.log")
assert_contains "$create_line" $'--base\tmain' "create targets the default branch"
assert_contains "$create_line" $'--head\t'"$branch" "create names the leased branch as head"
assert_contains "$(jq -r '.[0].title' "$FAKE_GH_STATE/prs.json")" "$id" "the PR title carries the story id"
assert_contains "$(jq -r '.[0].title' "$FAKE_GH_STATE/prs.json")" "Submit from the lease" "the PR title carries the story title"
assert_contains "$(jq -r '.[0].body' "$FAKE_GH_STATE/prs.json")" "$id" "the PR body references the story"
assert_contains "$(jqf "$out" .display)" "pull/100" "the display names the pull request"

# --- resubmission: push is a no-op, the open PR is adopted, no second create ------
out=$(submit); status=$?
assert_eq "$status" "0" "resubmitting an unchanged branch succeeds"
assert_eq "$(jqf "$out" .pushed)" "false" "an unchanged branch is not pushed again"
assert_eq "$(jqf "$out" .pull_request.adopted)" "true" "the open pull request is adopted"
assert_eq "$(jqf "$out" .pull_request.number)" "100" "…the same one"
assert_eq "$(create_count)" "1" "adoption creates nothing"

# --- a new commit: pushed, still adopted -------------------------------------------
printf 'more\n' >>"$worktree/work.txt"
git -C "$worktree" -c user.name=t -c user.email=t@e commit -q -am "fix: more"
head2=$(git -C "$worktree" rev-parse HEAD)
out=$(submit); status=$?
assert_eq "$status" "0" "a fixed branch resubmits"
assert_eq "$(jqf "$out" .pushed)" "true" "the fix was pushed"
assert_eq "$(remote_tip)" "$head2" "origin/$branch carries the fix"
assert_eq "$(jqf "$out" .pull_request.adopted)" "true" "the fix rides the same pull request"
assert_eq "$(create_count)" "1" "still nothing created"

# --- a cross-repository PR for the same head is never adopted ----------------------
jq '[.[0] + {isCrossRepository:true, number:7, url:"https://github.com/fork/widgets/pull/7"}]' \
  "$FAKE_GH_STATE/prs.json" >"$FAKE_GH_STATE/prs.tmp" && mv -f "$FAKE_GH_STATE/prs.tmp" "$FAKE_GH_STATE/prs.json"
out=$(submit); status=$?
assert_eq "$status" "0" "a fork's pull request does not block submission"
assert_eq "$(jqf "$out" .pull_request.adopted)" "false" "a fork's pull request is not adopted"
assert_eq "$(create_count)" "2" "a same-repository pull request is opened beside it"
assert_eq "$(jqf "$out" .pull_request.number)" "101" "…with the next number"

# --- gh unreachable is infrastructure, never the agent's -----------------------------
out=$(submit FAKE_GH_FAIL="error connecting to api.github.com"); status=$?
assert_eq "$status" "1" "a failing gh fails the submission"
assert_eq "$(jqf "$out" .reason)" "pull-request-unlisted" "…by name"
assert_eq "$(jqf "$out" .class)" "infrastructure" "…as infrastructure, not repair"
assert_contains "$(jqf "$out" .display)" "api.github.com" "…carrying gh's own words"

# --- crash mid-submit: the PR exists, the URL never arrived; the replay adopts it ------
printf '[]\n' >"$FAKE_GH_STATE/prs.json"
printf 'again\n' >>"$worktree/work.txt"
git -C "$worktree" -c user.name=t -c user.email=t@e commit -q -am "fix: again"
out=$(submit FAKE_GH_FAIL_AFTER_CREATE=1); status=$?
assert_eq "$status" "1" "a gh that dies after creating fails this run"
assert_eq "$(jqf "$out" .reason)" "pull-request-uncreated" "…by name"
assert_eq "$(jqf "$out" .class)" "infrastructure" "…as infrastructure"
assert_eq "$(remote_tip)" "$(git -C "$worktree" rev-parse HEAD)" "the push had already landed"
before=$(create_count)
out=$(submit); status=$?
assert_eq "$status" "0" "the replay succeeds"
assert_eq "$(jqf "$out" .pull_request.adopted)" "true" "the replay adopts the pull request the crash left behind"
assert_eq "$(create_count)" "$before" "the replay creates nothing"

# --- a rewritten branch is the agent's to reconcile, never force-pushed ----------------
git -C "$worktree" -c user.name=t -c user.email=t@e commit -q --amend -m "fix: rewritten"
out=$(submit); status=$?
assert_eq "$status" "1" "a non-fast-forward push is refused"
assert_eq "$(jqf "$out" .reason)" "push-rejected" "…by name"
assert_eq "$(jqf "$out" .class)" "repair" "…as the agent's to repair"
[ "$(remote_tip)" != "$(git -C "$worktree" rev-parse HEAD)" ] || fail_test "the rewritten tip was force-pushed"
git -C "$worktree" reset -q --hard "$(remote_tip)"

# --- the lease is proved, not trusted -------------------------------------------------
wrong=$(printf '%s' "$lease" | jq '.branch = "worktree-elsewhere"')
out=$(cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$wrong" PATH="$FAKES_PATH" \
  bash "$SCRIPT" --project "$slug" submit "$id" 2>&1)
assert_eq "$(jqf "$out" .reason)" "cleanup-lease-worktree-mismatch" "a lease naming another branch is refused"
other=$(new_story "$repo" "Another story")
out=$(cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" PATH="$FAKES_PATH" \
  bash "$SCRIPT" --project "$slug" submit "$other" 2>&1)
assert_eq "$(jqf "$out" .reason)" "cleanup-lease-story-mismatch" "a lease for another story is refused"

# --- SH-691: the base is origin's ADVERTISED default, never the local cache -----------
# The fixture's cache (refs/remotes/origin/HEAD) says `main`; origin itself is
# moved to `dev`. Five real pull requests reached `main` this way.
printf '[]\n' >"$FAKE_GH_STATE/prs.json"
git -C "$repo" push -q origin main:refs/heads/dev
git --git-dir="$origin" symbolic-ref HEAD refs/heads/dev
assert_eq "$(git -C "$repo" symbolic-ref refs/remotes/origin/HEAD)" "refs/remotes/origin/main" \
  "fixture: the local origin/HEAD cache still says main"
before=$(create_count)
out=$(submit); status=$?
assert_eq "$status" "0" "a stale cache does not stop submission: $out"
assert_eq "$(jqf "$out" .pull_request.base)" "dev" "the pull request targets origin's advertised default, not the cached one"
assert_eq "$(create_count)" "$((before + 1))" "one pull request was opened"
assert_contains "$(grep $'^pr\tcreate\t' "$FAKE_GH_STATE/argv.log" | tail -n 1)" $'--base\tdev' \
  "create names origin's default as the base"
assert_contains "$(jqf "$out" .display)" "against dev" "the display names the base"

# --- SH-691: an ABSENT cache is not evidence of `main` ---------------------------------
# The mutation check for the retired literal fallback: with `printf 'main'`
# restored in lib/session.sh, this case opens the pull request against main.
printf '[]\n' >"$FAKE_GH_STATE/prs.json"
git -C "$repo" remote set-head origin --delete
if git -C "$repo" symbolic-ref --quiet refs/remotes/origin/HEAD >/dev/null 2>&1; then
  fail_test "fixture: the local origin/HEAD cache should be gone"
fi
before=$(create_count)
out=$(submit); status=$?
assert_eq "$status" "0" "no cache does not stop submission: $out"
assert_eq "$(jqf "$out" .pull_request.base)" "dev" "with no cache the base is still asked of origin"
assert_contains "$(grep $'^pr\tcreate\t' "$FAKE_GH_STATE/argv.log" | tail -n 1)" $'--base\tdev' \
  "create asked origin; it did not assume main"
assert_eq "$(create_count)" "$((before + 1))" "one pull request was opened"

# --- SH-691: an origin that advertises no default is refused, nothing pushed -----------
printf 'unknown\n' >>"$worktree/work.txt"
git -C "$worktree" -c user.name=t -c user.email=t@e commit -q -am "fix: unknown default"
unpushed=$(git -C "$worktree" rev-parse HEAD)
git --git-dir="$origin" update-ref --no-deref HEAD "$(git --git-dir="$origin" rev-parse refs/heads/dev)"
before=$(create_count)
out=$(submit); status=$?
assert_eq "$status" "1" "a detached origin HEAD refuses the submission"
assert_eq "$(jqf "$out" .reason)" "default-branch-unknown" "…by name"
assert_eq "$(jqf "$out" .class)" "infrastructure" "…as the verifier's incident, not the agent's"
assert_contains "$(jqf "$out" .display)" "no symbolic HEAD" "…saying what origin advertised"
[ "$(remote_tip)" != "$unpushed" ] || fail_test "an unknown default must not push"
assert_eq "$(create_count)" "$before" "an unknown default opens nothing"
git --git-dir="$origin" symbolic-ref HEAD refs/heads/dev

# --- SH-691: an unreachable origin is refused by name, before any push -----------------
git -C "$repo" remote set-url origin /nonexistent/storyhook-origin.git
out=$(submit); status=$?
assert_eq "$status" "1" "an unreachable origin refuses the submission"
assert_eq "$(jqf "$out" .reason)" "default-branch-unknown" "…by name"
assert_eq "$(jqf "$out" .class)" "infrastructure" "…as infrastructure"
assert_contains "$(jqf "$out" .display)" "did not answer" "…carrying git's own words"
git -C "$repo" remote set-url origin "$origin"
[ "$(remote_tip)" != "$unpushed" ] || fail_test "an unreachable origin cannot have been pushed to"
assert_eq "$(create_count)" "$before" "nothing was opened while origin was unreachable"

# --- the verb is a router verb the agent-facing docs and usage name --------------------
assert_contains "$(router_verbs "$SCRIPT")" "submit" "submit is a router verb"
usage=$(jqf "$(bash "$SCRIPT" bogus-subcommand 2>&1)" .display)
assert_contains "$usage" "submit <story-id>" "usage names submit"

finish
