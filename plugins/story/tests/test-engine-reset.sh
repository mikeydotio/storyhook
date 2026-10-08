#!/usr/bin/env bash
# Real daemon -> engine -> shared native Git/tmux reset. Only operational lane
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

# Wait only for the protocol's transient ownership states. An optional lock
# proves the first request refuses a real competing workspace owner.
stop_now() {
  python3 - "$repo" "$1" "$TESTS_DIR" "${2:-}" <<'PYWAIT'
import fcntl,json,pathlib,subprocess,sys,time
repo,run,tests,lock_path=sys.argv[1:]
sys.path.insert(0,str(pathlib.Path(tests).parents[2]/"scripts/tests"))
import load_grace
allowance=load_grace.patience(120,load_grace.contention())
deadline=time.monotonic()+allowance
lock=None
if lock_path:
    pathlib.Path(lock_path).parent.mkdir(parents=True,exist_ok=True)
    lock=open(lock_path,"a")
    fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
try:
    while True:
        remaining=deadline-time.monotonic()
        if remaining<=0:
            sys.exit(f"Stop Now did not settle within {allowance}s; last response: {result.stdout}")
        result=subprocess.run(["story","engine","stop","--run",run,"--now","--json"],
            cwd=repo,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=remaining)
        answer=json.loads(result.stdout)
        busy=answer.get("result")=="error" and "workspace is busy" in answer.get("error","")
        if lock is not None:
            assert busy,result.stdout
            lock.close()
            lock=None
        draining=answer.get("result")=="ok" and answer.get("run",{}).get("state")=="draining"
        if not busy and not draining:
            print(result.stdout,end="")
            sys.exit(result.returncode)
        print(f"Stop Now awaits workspace settlement: {result.stdout.strip()}",file=sys.stderr)
        time.sleep(min(0.1,max(0,deadline-time.monotonic())))
finally:
    if lock is not None:
        lock.close()
PYWAIT
}

active=$(new_story "$repo" "discard unfinished work")
verify=$(new_story "$repo" "preserve verification")
unrelated=$(new_story "$repo" "preserve unrelated active work")
active_wt=$(make_owned "$active")
verify_wt=$(make_owned "$verify")
unrelated_wt=$(make_owned "$unrelated")
(cd "$repo" && story claim "$active" >/dev/null && story claim "$unrelated" >/dev/null) || exit 1
# Stopped verification still processes submissions without tests. Hold this
# handoff for a person so verifier recovery cannot race the engine reset.
(cd "$repo" && story label "$verify" human-only >/dev/null) || exit 1
(cd "$verify_wt" && story move "$verify" verifying >/dev/null) || exit 1
printf 'untracked work\n' > "$active_wt/untracked.txt"
printf 'committed work\n' > "$active_wt/work.txt"
git -C "$active_wt" add work.txt
git -C "$active_wt" commit -qm unfinished
git -C "$repo" worktree lock "$active_wt" --reason 'fixture explicit lock'
verify_before=$(cd "$repo" && story show "$verify" --json | jq -c '.story.story')
seed_run sh706-reset-real "$active_wt" "$verify_wt"
out=$(stop_now sh706-reset-real)
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

# A mismatched marker leaves every resource and finishes with a dispatch hold.
bad=$(new_story "$repo" 'changed resource identity')
bad_wt=$(make_owned "$bad")
(cd "$repo" && story claim "$bad" >/dev/null) || exit 1
seed_run sh706-reset-marker "$bad_wt"
private=$(git -C "$bad_wt" rev-parse --absolute-git-dir)
marker="$private/storyhook-cleanup-lease-v1.json"
cp "$marker" "$socket_root/original-marker.json"
jq '.branch="foreign-branch"' "$socket_root/original-marker.json" > "$marker"
out=$(stop_now sh706-reset-marker "$repo/.git/storyhook/workspace-locks/$bad.lock")
assert_eq "$(jqf "$out" .run.state)" finished 'uncertain lane settles with residue'
assert_contains "$out" 'marker' 'marker mismatch identifies its resource'
assert_contains "$(tmux -S "$socket" list-windows -a -F '#{window_name}')" "$bad" 'mismatched target window preserved'
[ -d "$bad_wt" ] || fail_test 'mismatched worktree removed'
git -C "$repo" show-ref --verify --quiet "refs/heads/worktree-$bad" || fail_test 'mismatched branch removed'
assert_eq "$(cd "$repo" && story show "$bad" --json | jq -r '.story.story.state')" todo 'uncertain reset restores prior state'
assert_contains "$(cd "$repo" && story show "$bad" --json | jq -r '.story.story.awaiting')" 'marker' 'retained identity blocks redispatch'

# Installed artifacts are retained and reported by the same native teardown.
installed=$(new_story "$repo" 'preserve installed artifacts')
installed_wt=$(make_owned "$installed")
(cd "$repo" && story claim "$installed" >/dev/null) || exit 1
seed_run sh890-reset-installed "$installed_wt"
[ ! -e "$STORYHOOK_DATA_DIR/managed-paths" ] || exit 1
printf '%s\n' "$installed_wt/installed-plugin" > "$STORYHOOK_DATA_DIR/managed-paths"
out=$(stop_now sh890-reset-installed)
assert_eq "$(jqf "$out" .run.state)" finished 'installed resource lane settles with residue'
assert_contains "$out" 'installed' 'leased reset records installed-resource protection'
[ -d "$installed_wt" ] || fail_test 'installed-resource worktree removed'
git -C "$repo" show-ref --verify --quiet "refs/heads/worktree-$installed" || fail_test 'installed-resource branch removed'
assert_contains "$(cd "$repo" && story show "$installed" --json | jq -r '.story.story.awaiting')" 'installed' 'retained installed resources block redispatch'
rm "$STORYHOOK_DATA_DIR/managed-paths"

# A partial prior cleanup may leave only its exact window and local branch.
# Absence is observed from the original lease, without rediscovering other work.
partial=$(new_story "$repo" 'restart between deletion and finalization')
partial_wt=$(make_owned "$partial")
(cd "$repo" && story claim "$partial" >/dev/null) || exit 1
seed_run sh706-reset-partial "$partial_wt"
git -C "$repo" worktree remove --force "$partial_wt" || exit 1
out=$(stop_now sh706-reset-partial)
assert_eq "$(jqf "$out" .run.state)" finished 'absent worktree cleanup finishes'
git -C "$repo" show-ref --verify --quiet "refs/heads/worktree-$partial" && fail_test 'partial reset branch survived'
assert_eq "$(cd "$repo" && story show "$partial" --json | jq -r '.story.story.state')" todo 'partial reset restores only after remaining cleanup'
windows=$(tmux -S "$socket" list-windows -a -F '#{window_name}')
printf '%s\n' "$windows" | rg -x "$partial" && fail_test 'partial reset window survived'

exit "$_FAILED"
