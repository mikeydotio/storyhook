#!/usr/bin/env bash
# SH-584: a failed tmux query is not evidence that the agent window is absent.
source "$(dirname "$0")/lib.sh"

fake_bin=$(mktemp -d /tmp/story-test.XXXXXX)
_register_tmp "$fake_bin"
cat >"$fake_bin/tmux" <<'TMUX'
#!/usr/bin/env bash
if [ "${1:-}" = list-panes ]; then
  if [ "${NOTIFY_QUERY_RESULT:-error}" = empty ]; then exit 0; fi
  printf 'error connecting to /fixture/vanished-server (No such file or directory)\n' >&2
  exit 1
fi
printf 'unexpected tmux call: %s\n' "$*" >&2
exit 64
TMUX
chmod 700 "$fake_bin/tmux"

out=$(PATH="$fake_bin:$PATH" TMUX='/fixture/vanished-server,12,0' \
  bash "$SCRIPT" notify SH-584 'must not be delivered' 2>&1)
assert_eq "$(jqf "$out" .ok)" false 'lookup failure refuses delivery'
assert_eq "$(jqf "$out" .reason)" pane-query-failed 'lookup failure is not a missing pane'
assert_contains "$(jqf "$out" .display)" '/fixture/vanished-server' 'failure identifies queried server'
assert_contains "$(jqf "$out" .display)" 'error connecting' 'failure preserves tmux diagnostic'

out=$(PATH="$fake_bin:$PATH" NOTIFY_QUERY_RESULT=empty TMUX='/fixture/other-server,12,0' \
  bash "$SCRIPT" notify SH-584 'must not be delivered' 2>&1)
assert_eq "$(jqf "$out" .reason)" pane-unavailable 'successful empty lookup reports absence'
assert_contains "$(jqf "$out" .display)" '/fixture/other-server' 'absence is scoped to queried server'
finish
