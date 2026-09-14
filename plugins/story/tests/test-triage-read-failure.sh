#!/usr/bin/env bash
# SH-687: external query failures must not become a clean backlog report.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
real_story=$(command -v story)
proxy=$(mktemp -d /tmp/story-test.triage-proxy.XXXXXX)
_register_tmp "$proxy"
cat >"$proxy/story" <<'PROXY'
#!/usr/bin/env bash
set -uo pipefail
args=("$@")
while [ "$#" -gt 0 ]; do
  case "$1" in
    --project|--actor) shift 2 ;;
    --project=*|--actor=*) shift ;;
    *) break ;;
  esac
done
if [ "${1:-}" = list ]; then
  kind=all
  case " $* " in
    *' --blocked '*) kind=blocked ;;
    *' --stale '*) kind=stale ;;
  esac
  if [ "$kind" = "$TRIAGE_FAIL_QUERY" ]; then
    case "$TRIAGE_FAIL_MODE" in
      process) printf 'query transport unavailable\n' >&2; exit 5 ;;
      error) printf '{"result":"error","error":"query transport unavailable"}\n'; exit 0 ;;
      malformed) printf 'not JSON\n'; exit 0 ;;
      shape) printf '{"result":"ok","stories":{}}\n'; exit 0 ;;
      absent) printf '{"result":"ok"}\n'; exit 0 ;;
    esac
  fi
fi
exec "$TRIAGE_REAL_STORY" "${args[@]}"
PROXY
chmod +x "$proxy/story"

for query in all stale blocked; do
  for mode in process error malformed shape absent; do
    out=$(cd "$repo" && PATH="$proxy:$PATH" TRIAGE_REAL_STORY="$real_story" \
      TRIAGE_FAIL_QUERY="$query" TRIAGE_FAIL_MODE="$mode" bash "$SCRIPT" triage 2>&1)
    assert_eq "$(jqf "$out" .ok)" false "$query/$mode: no successful findings"
    if [ "$mode" = process ] || [ "$mode" = error ]; then
      assert_contains "$(jqf "$out" .display)" 'query transport unavailable' \
        "$query/$mode: original diagnosis survives"
    fi
  done
done
finish
