#!/usr/bin/env bash
# Real daemon -> engine -> helper -> Git/tmux reset. Only operational lane
# rows are fixture data; state transitions, cleanup and receipts are production.
real_tmux=$(command -v tmux)
source "$(dirname "$0")/lib.sh"
export STORYHOOK_DISPATCH_SCRIPT="$SCRIPT"
socket_root=$(mktemp -d /tmp/story-test-engine-reset.XXXXXX)
_TMP_REPOS+=("$socket_root")
socket="$socket_root/tmux.sock"
mkdir "$socket_root/bin"
printf '#!/usr/bin/env bash\nexec %q -S %q "$@"\n' "$real_tmux" "$socket" > "$socket_root/bin/tmux"
chmod +x "$socket_root/bin/tmux"
export PATH="$socket_root/bin:$PATH"
tmux -S "$socket" -f /dev/null new-session -d -s story-test-engine-reset -n keeper 'exec cat' || exit 1
engine_cleanup() {
  local status=$?
  trap - EXIT
  story daemon stop --force >/dev/null 2>&1 || status=1
  tmux -S "$socket" kill-server >/dev/null 2>&1 || status=1
  (exit "$status")
  _cleanup
}
trap engine_cleanup EXIT

repo=$(mk_story_repo)
repo=$(cd "$repo" && pwd -P)
slug=$(slug_for "$repo")
(cd "$repo" && story verifier stop >/dev/null) || exit 1

make_owned() {
  local id="$1" w wt private
  w=$(mk_dispatched "$repo" "$id")
  wt=$(cd "$repo/.claude/worktrees/$w" && pwd -P)
  private=$(git -C "$wt" rev-parse --absolute-git-dir)
  jq -n --arg project "$slug" --arg id "$id" --arg repo "$repo" --arg wt "$wt" --arg branch "worktree-$w" --arg socket "$socket" \
    '{version:1,project_slug:$project,story_id:$id,repository_path:$repo,worktree_path:$wt,branch:$branch,tmux:{socket_path:$socket}}' > "$private/storyhook-cleanup-lease-v1.json"
  tmux -S "$socket" new-window -d -t story-test-engine-reset -n "$id" -c "$wt" 'exec cat'
  printf '%s' "$wt"
}

seed_run() {
  local run="$1"
  shift
  python3 - "$STORYHOOK_STORE_PATH" "$slug" "$run" "$@" <<'PY'
import datetime,json,sqlite3,subprocess,sys
db,slug,run,*targets=sys.argv[1:]
now=datetime.datetime.now(datetime.timezone.utc).isoformat()
with sqlite3.connect(db) as conn:
    conn.execute("PRAGMA foreign_keys=ON")
    conn.execute("INSERT INTO engine_runs (id,project_slug,scope_kind,lanes,agent,state,created_at,updated_at) VALUES (?,?,'project',?,'codex','paused',?,?)",(run,slug,len(targets),now,now))
    for index,worktree in enumerate(targets):
        private=subprocess.check_output(['git','-C',worktree,'rev-parse','--absolute-git-dir'],text=True).strip()
        with open(private+'/storyhook-cleanup-lease-v1.json') as handle: lease=json.load(handle)
        conn.execute("INSERT INTO engine_lanes (run_id,lane_index,state,story_id,window_name,worktree_path,dispatched_at,last_observed_at,cleanup_lease_json) VALUES (?,?,'working',?,?,?,?,?,?)",(run,index,lease['story_id'],lease['story_id'],worktree,now,now,json.dumps(lease)))
PY
}

active=$(new_story "$repo" "discard unfinished work")
verify=$(new_story "$repo" "preserve verification")
unrelated=$(new_story "$repo" "preserve unrelated active work")
active_wt=$(make_owned "$active")
verify_wt=$(make_owned "$verify")
unrelated_wt=$(make_owned "$unrelated")
(cd "$repo" && story claim "$active" >/dev/null && story claim "$unrelated" >/dev/null) || exit 1
(cd "$verify_wt" && story move "$verify" verifying >/dev/null) || exit 1
printf 'untracked work\n' > "$active_wt/untracked.txt"
printf 'committed work\n' > "$active_wt/work.txt"
git -C "$active_wt" add work.txt
git -C "$active_wt" commit -qm unfinished
git -C "$repo" worktree lock "$active_wt" --reason 'fixture explicit lock'
verify_before=$(cd "$repo" && story show "$verify" --json | jq -c '.story.story')
seed_run sh706-reset-real "$active_wt" "$verify_wt"
out=$(cd "$repo" && story engine stop --run sh706-reset-real --now --json 2>&1)
if [ "$(jqf "$out" .result)" != ok ]; then
  fail_test "real reset returned: $out"
  exit 1
fi
assert_eq "$(jqf "$out" .run.state)" finished "real reset finishes the run"
[ ! -e "$active_wt" ] || fail_test 'dirty locked worktree survived reset'
git -C "$repo" show-ref --verify --quiet "refs/heads/worktree-$active" && fail_test 'unpushed local branch survived reset'
assert_eq "$(cd "$repo" && story show "$active" --json | jq -r '.story.story.state')" todo 'story restored after cleanup'
assert_eq "$(cd "$repo" && story show "$active" --json | jq -r '.story.story.awaiting')" null 'intentional stop did not block story'
assert_eq "$(cd "$repo" && story show "$verify" --json | jq -c '.story.story')" "$verify_before" 'verification history and state preserved'
[ -d "$verify_wt" ] || fail_test 'verifier worktree removed'
[ -d "$unrelated_wt" ] || fail_test 'unrelated worktree removed'
windows=$(tmux -S "$socket" list-windows -a -F '#{window_name}')
assert_contains "$windows" "$verify" 'verifier window preserved'
assert_contains "$windows" "$unrelated" 'unrelated window preserved'
printf '%s\n' "$windows" | rg -x "$active" && fail_test 'cancelled window survived'

# A mismatched marker refuses before closing the window or deleting Git work.
bad=$(new_story "$repo" 'changed resource identity')
bad_wt=$(make_owned "$bad")
(cd "$repo" && story claim "$bad" >/dev/null) || exit 1
seed_run sh706-reset-marker "$bad_wt"
private=$(git -C "$bad_wt" rev-parse --absolute-git-dir)
marker="$private/storyhook-cleanup-lease-v1.json"
cp "$marker" "$socket_root/original-marker.json"
jq '.branch="foreign-branch"' "$socket_root/original-marker.json" > "$marker"
out=$(cd "$repo" && story engine stop --run sh706-reset-marker --now --json 2>&1)
assert_contains "$out" 'cleanup lease marker' 'marker mismatch identifies its resource'
assert_contains "$out" 'branch mismatch' 'marker mismatch is diagnosed'
assert_contains "$(tmux -S "$socket" list-windows -a -F '#{window_name}')" "$bad" 'mismatched target window preserved'
[ -d "$bad_wt" ] || fail_test 'mismatched worktree removed'
assert_eq "$(cd "$repo" && story show "$bad" --json | jq -r '.story.story.state')" in-progress 'failed reset retains active claim'
check=$(cd "$repo" && story engine reset-check "$bad" --json 2>&1)
assert_contains "$check" 'reset in progress' 'dispatch guard sees reservation'
cp "$socket_root/original-marker.json" "$marker"
# An installed artifact nested inside the exact leased worktree remains a
# refusal even though the operator confirmed discarding ordinary lane work.
[ ! -e "$STORYHOOK_DATA_DIR/managed-paths" ] || exit 1
printf '%s\n' "$bad_wt/installed-plugin" > "$STORYHOOK_DATA_DIR/managed-paths"
out=$(cd "$repo" && story engine stop --run sh706-reset-marker --now --json 2>&1)
assert_contains "$out" 'overlaps installed artifacts' 'leased reset preserves installed resources'
[ -d "$bad_wt" ] || fail_test 'installed-resource worktree removed'
rm "$STORYHOOK_DATA_DIR/managed-paths"
out=$(cd "$repo" && story engine stop --run sh706-reset-marker --now --json 2>&1)
assert_eq "$(jqf "$out" .run.state)" finished 'same reserved operation retries successfully'
[ ! -e "$bad_wt" ] || fail_test 'repaired exact target was not removed'

# Recovery accepts Git cleanup already completed before finalization while
# still closing the remaining exact window and deleting the remaining branch.
partial=$(new_story "$repo" 'restart between deletion and finalization')
partial_wt=$(make_owned "$partial")
(cd "$repo" && story claim "$partial" >/dev/null) || exit 1
seed_run sh706-reset-partial "$partial_wt"
private=$(git -C "$partial_wt" rev-parse --absolute-git-dir)
marker="$private/storyhook-cleanup-lease-v1.json"
cp "$marker" "$socket_root/partial-marker.json"
jq '.branch="foreign-branch"' "$socket_root/partial-marker.json" > "$marker"
out=$(cd "$repo" && story engine stop --run sh706-reset-partial --now --json 2>&1)
assert_contains "$out" 'cleanup lease marker' 'partial fixture names its mismatched marker'
assert_contains "$out" 'branch mismatch' 'partial fixture holds a durable reservation'
git -C "$repo" worktree remove --force "$partial_wt" || exit 1
out=$(cd "$repo" && story engine stop --run sh706-reset-partial --now --json 2>&1)
assert_eq "$(jqf "$out" .run.state)" finished 'absent worktree retry finishes'
git -C "$repo" show-ref --verify --quiet "refs/heads/worktree-$partial" && fail_test 'partial reset branch survived'
assert_eq "$(cd "$repo" && story show "$partial" --json | jq -r '.story.story.state')" todo 'partial reset restores only after remaining cleanup'

exit "$_FAILED"
