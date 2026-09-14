#!/usr/bin/env bash
# Every dispatch mode must commit revocation before a provider replacement.
source "$(dirname "$0")/lib.sh"

real_story=$(command -v story)
endpoint_root=$(mktemp -d /tmp/story-test-delivery-endpoint.XXXXXX)
_TMP_REPOS+=("$endpoint_root")
cat > "$endpoint_root/story" <<'PY'
#!/usr/bin/env python3
import fcntl,json,os,subprocess,sys
from pathlib import Path
args=sys.argv[1:]
if "supersede-block-deliveries" in args:
    project=args[args.index("--project")+1]
    pos=args.index("supersede-block-deliveries")
    story=args[pos+1]
    assert args[pos-1:]==["internal","supersede-block-deliveries",story,"--json"], args
    fd=int(os.environ["STORY_WORKSPACE_LOCK_FD"])
    common=subprocess.check_output(["git","rev-parse","--path-format=absolute","--git-common-dir"],text=True).strip()
    path=Path(common)/"storyhook/workspace-locks"/(story+".lock")
    actual=os.fstat(fd)
    expected=path.stat()
    assert (actual.st_dev,actual.st_ino)==(expected.st_dev,expected.st_ino)
    with path.open("a+") as competitor:
        try: fcntl.flock(competitor,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError: pass
        else: raise AssertionError("revocation endpoint called without workspace exclusion")
    Path(os.environ["REVOCATION_CALLED"]).write_text(story)
    print(json.dumps(dict(protocol_version=1,project=project,story_id=story,superseded=-1)))
    sys.exit(0)
os.execv(os.environ["REAL_STORY_ENDPOINT"],[os.environ["REAL_STORY_ENDPOINT"]]+args)
PY
chmod +x "$endpoint_root/story"

for mode in fresh force resume next; do
  repo=$(mk_story_repo)
  id=$(new_story "$repo" "Revocation boundary $mode")
  before=todo
  args=("$id")
  case "$mode" in
    force) (cd "$repo" && story move "$id" in-progress >/dev/null); args+=(--force); before=in-progress ;;
    resume)
      (cd "$repo" && story move "$id" in-progress >/dev/null)
      mk_dispatched "$repo" "$id" >/dev/null
      args+=(--resume --agent=claude)
      before=in-progress ;;
    next) args=(--next) ;;
  esac
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-delivery-tmux.XXXXXX)
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  out=$(cd "$repo" && STORY_BIN="$endpoint_root/story" REAL_STORY_ENDPOINT="$real_story" \
    REVOCATION_CALLED="$endpoint_root/$mode" TMUX="fake,0,0" TMUX_PANE=%0 \
    STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 STORY_CONFIRM_DELAY=0 \
    STORY_PASTE_SETTLE_DELAY=0 STORY_COUNCIL=off bash "$SCRIPT" dispatch "${args[@]}" 2>&1)
  assert_eq "$(jqf "$out" .ok)" false "$mode refuses malformed durable receipt"
  assert_contains "$(jqf "$out" .display)" 'revocation receipt' "$mode reached the checked boundary"
  [ -f "$endpoint_root/$mode" ] || fail_test "$mode never called the revocation endpoint"
  for effect in new_window_args.log respawn_pane_args.log submitted; do
    [ ! -e "$FAKE_TMUX_STATE/$effect" ] || fail_test "$mode exposed a provider effect: $effect"
  done
  assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "$before" "$mode preserves the prior claim"
  if [ "$mode" = resume ]; then
    [ -d "$repo/.claude/worktrees/$id" ] || fail_test "resume removed retained work"
  else
    [ ! -e "$repo/.claude/worktrees/$id" ] || fail_test "$mode created resources after refusal"
  fi
done
finish
