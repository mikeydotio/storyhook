#!/usr/bin/env bash
# SH-584: a failed tmux query is not evidence that the agent window is absent.
source "$(dirname "$0")/lib.sh"

fake_bin=$(mktemp -d /tmp/story-test.XXXXXX)
_register_tmp "$fake_bin"
cat >"$fake_bin/tmux" <<'TMUX'
#!/usr/bin/env bash
[ "${1:-}" = -S ] || exit 64
socket="$2"
shift 2
if [ "${1:-}" = list-panes ]; then
  case "$socket" in */empty.sock) exit 0 ;; esac
  printf 'error connecting to %s (Permission denied)\n' "$socket" >&2
  exit 1
fi
exit 64
TMUX
chmod 700 "$fake_bin/tmux"
export PATH="$fake_bin:$PATH"
repo=$(mk_story_repo)
id=$(new_story "$repo" "Terminal query")
touch "$fake_bin/error.sock" "$fake_bin/empty.sock"

out=$(cd "$repo" && TMUX="$fake_bin/error.sock,12,0" \
  bash "$SCRIPT" notify "$id" 'must not be delivered' 2>&1)
assert_eq "$(jqf "$out" .ok)" false 'lookup failure refuses delivery'
assert_eq "$(jqf "$out" .reason)" resource-identity-unsafe 'lookup failure is not a missing pane'
assert_contains "$(jqf "$out" .display)" error.sock 'failure identifies queried server'
assert_contains "$(jqf "$out" .display)" 'Permission denied' 'failure preserves tmux diagnostic'

out=$(cd "$repo" && TMUX="$fake_bin/empty.sock,12,0" \
  bash "$SCRIPT" notify "$id" 'must not be delivered' 2>&1)
assert_eq "$(jqf "$out" .reason)" pane-unavailable 'successful empty lookup reports absence'
assert_contains "$(jqf "$out" .display)" empty.sock 'absence is scoped to queried server'
finish
