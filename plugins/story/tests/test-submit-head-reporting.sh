#!/usr/bin/env bash
# SH-713: the submission receipt identifies the committed head actually pushed
# to the real remote even while GitHub's PR metadata still reports an older one.
source "$(dirname "$0")/lib.sh"

if [ -n "${SH713_RECEIPTS_PATH:-}" ]; then
  case "$SH713_RECEIPTS_PATH" in
    /tmp/*) [ -f "$SH713_RECEIPTS_PATH" ] || { fail_test "receipt capture must be a caller-owned existing file"; finish; } ;;
    *) fail_test "receipt capture must be inside /tmp"; finish ;;
  esac
fi

FAKE_GH_STATE="$(mktemp -d /tmp/story-test-gh.XXXXXX)"
export FAKE_GH_STATE
_TMP_REPOS+=("$FAKE_GH_STATE")
mkdir "$FAKE_GH_STATE/bin"
# Only the external pr-view JSON changes. The helper, Git push, remote reads,
# lease checks, and ordinary fake endpoint state transitions all remain real.
cat >"$FAKE_GH_STATE/bin/gh" <<'WRAPPER'
#!/usr/bin/env bash
set -uo pipefail
reply=$("$SH713_BASE_GH" "$@") || exit $?
if [ "${1:-} ${2:-}" = "pr view" ] && [ -n "${SH713_STALE_VIEW_HEAD:-}" ]; then
  printf '%s' "$reply" | jq -c --arg oid "$SH713_STALE_VIEW_HEAD" '.headRefOid = $oid'
else
  printf '%s\n' "$reply"
fi
WRAPPER
chmod +x "$FAKE_GH_STATE/bin/gh"

repo=$(mk_story_repo HEAD)
slug=$(slug_for "$repo")
id=$(new_story "$repo" "Report the submitted commit")
name=$(mk_dispatched "$repo" "$id")
worktree="$repo/.claude/worktrees/$name"
branch="worktree-$name"
repository_path=$(cd "$repo" && pwd -P)
worktree_path=$(cd "$worktree" && pwd -P)
lease=$(jq -n --arg project "$slug" --arg story "$id" \
  --arg repository "$repository_path" --arg worktree "$worktree_path" \
  --arg branch "$branch" --arg socket "$FAKE_TMUX_STATE/tmux.sock" \
  '{version:1,project_slug:$project,story_id:$story,repository_path:$repository,
    worktree_path:$worktree,branch:$branch,tmux:{socket_path:$socket}}')
real_story=$(command -v story)
verifying_path="$FAKE_GH_STATE/bin:$TESTS_DIR/fakes/story-verifying:$TESTS_DIR/fakes:$PATH"

submit() {
  (cd "$repo" && env -u STORY_AGENT STORYHOOK_REAP_LEASE_V1="$lease" \
    GH_PROMPT_DISABLED=1 PATH="$verifying_path" STORY_REAL_BIN="$real_story" \
    STORY_SHOW_AS_VERIFYING="$id" SH713_BASE_GH="$TESTS_DIR/fakes/gh" "$@" \
    bash "$SCRIPT" --project "$slug" submit "$id" 2>&1)
}

commit_work() {
  printf '%s\n' "$1" >>"$worktree/work.txt"
  git -C "$worktree" add work.txt
  git -C "$worktree" -c user.name=t -c user.email=t@e commit -q -m "fix: $1"
}

# Observe the remote independently after the helper returns. Failed assertions
# do not stop later scenarios, so a RED run still preserves every real receipt.
check_submission() {
  local scenario="$1" expected_pushed="$2" expected_adopted="$3" api_head="$4"
  shift 4
  local receipt status head remote
  receipt=$(submit "$@"); status=$?
  assert_eq "$status" "0" "$scenario: helper succeeds: $receipt"
  assert_eq "$(jqf "$receipt" .ok)" "true" "$scenario: successful receipt"
  head=$(git -C "$worktree" rev-parse HEAD)
  remote=$(git -C "$repo" ls-remote --heads origin "$branch" | cut -f1)
  assert_eq "$remote" "$head" "$scenario: independently checked remote equals committed HEAD"
  assert_eq "$(jqf "$receipt" .pushed)" "$expected_pushed" "$scenario: push path"
  assert_eq "$(jqf "$receipt" .pull_request.adopted)" "$expected_adopted" "$scenario: adoption path"
  assert_eq "$(jqf "$receipt" .pull_request.head_oid)" "$remote" \
    "$scenario: receipt must name the submitted remote head, API reports $api_head"
  if [ -n "${SH713_RECEIPTS_PATH:-}" ]; then
    jq -cn --arg scenario "$scenario" --arg expected_head "$remote" \
      --arg api_head "$api_head" --argjson receipt "$receipt" \
      '{scenario:$scenario,expected_head:$expected_head,api_head:$api_head,receipt:$receipt}' \
      >>"$SH713_RECEIPTS_PATH" || fail_test "$scenario: capture receipt"
  fi
}

commit_work "initial submission"
first_head=$(git -C "$worktree" rev-parse HEAD)
check_submission create-current true false "$first_head"
check_submission adopt-current false true "$first_head"

commit_work "fast-forward submission"
second_head=$(git -C "$worktree" rev-parse HEAD)
assert_eq "$(jq -r '.[0].headRefOid' "$FAKE_GH_STATE/prs.json")" "$first_head" \
  "fixture: existing PR API metadata remains at the old head"
check_submission adopt-fast-forward-lagging true true "$first_head"
check_submission adopt-unchanged-lagging false true "$first_head"

jq --arg oid "$second_head" 'map(.headRefOid = $oid)' "$FAKE_GH_STATE/prs.json" \
  >"$FAKE_GH_STATE/prs.next"
mv "$FAKE_GH_STATE/prs.next" "$FAKE_GH_STATE/prs.json"
check_submission adopt-caught-up false true "$second_head"

printf '[]\n' >"$FAKE_GH_STATE/prs.json"
commit_work "new pull request with lagging metadata"
check_submission create-lagging true false "$second_head" SH713_STALE_VIEW_HEAD="$second_head"

finish
