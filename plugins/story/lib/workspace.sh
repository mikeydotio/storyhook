# Shared exclusion for story session effects. Keep the open description in the
# shell and its effect children until quiescence; never explicitly unlock it.
reserve_story_workspace() {
  local workspace_id="$1" common lock_path inherited="${STORY_WORKSPACE_LOCK_FD:-}"
  common=$(git rev-parse --path-format=absolute --git-common-dir) || fail "cannot locate workspace lock directory"
  mkdir -p "$common/storyhook/workspace-locks" || fail "cannot create workspace lock directory"
  lock_path="$common/storyhook/workspace-locks/$workspace_id.lock"
  if [ -n "$inherited" ]; then
    case "$inherited" in *[!0-9]*) fail "invalid inherited workspace descriptor" ;; esac
    [ "$inherited" -ge 3 ] || fail "invalid inherited workspace descriptor"
    eval "exec 9<&$inherited" || fail "workspace ownership descriptor is unavailable"
  else
    [ ! -L "$lock_path" ] || fail "workspace lock is a symbolic link"
    exec 9>>"$lock_path" || fail "cannot open workspace lock"
  fi
  local lock_error
  if ! lock_error=$(python3 - "$lock_path" 2>&1 <<'PYLOCK'
import fcntl, os, stat, sys
expected = os.stat(sys.argv[1], follow_symlinks=False)
actual = os.fstat(9)
if not stat.S_ISREG(expected.st_mode) or (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino):
    sys.exit("workspace lock identity changed")
try:
    fcntl.flock(9, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    sys.exit("workspace is busy with reset, dispatch, verification, or notification")
PYLOCK
  ); then
    fail "$workspace_id: $lock_error"
  fi
  export STORY_WORKSPACE_LOCK_FD=9
}

# Only a committed, identity-bound receipt permits another session lifetime.
# The internal operation narrows authority and does not reacquire this lock.
BLOCK_DELIVERY_RECEIPT=""
BLOCK_DELIVERY_ERROR=""
supersede_block_deliveries() {
  local workspace_id="$1"
  BLOCK_DELIVERY_ERROR=""
  if ! BLOCK_DELIVERY_RECEIPT=$(story_cli internal supersede-block-deliveries "$workspace_id" --json); then
    BLOCK_DELIVERY_ERROR="could not retire pending delivery authority for $workspace_id: $BLOCK_DELIVERY_RECEIPT"
    return 1
  fi
  if ! printf '%s' "$BLOCK_DELIVERY_RECEIPT" | jq -e -s --arg project "$PROJECT_SLUG" --arg id "$workspace_id" '
    length == 1 and (.[0] | type == "object" and .protocol_version == 1 and .project == $project and .story_id == $id
    and (.superseded | type == "number" and . >= 0 and floor == .))
  ' >/dev/null 2>&1; then
    BLOCK_DELIVERY_ERROR="invalid pending-delivery revocation receipt for $workspace_id: $BLOCK_DELIVERY_RECEIPT"
    return 1
  fi
}
