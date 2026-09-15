#!/usr/bin/env bash
# SH-732: endpoint discovery must not depend on the displayed version's width.
source "$(dirname "$0")/lib.sh"
source "$PLUGIN_ROOT/lib/daemon-status.sh"

for version in '3.0.0' '3.0.0 (0)' '3.0.0 (272)' '3.0.0 (18446744073709551615)' '3.0.0-rc.1+local (272) (build abc123)'; do
  for port in 1 55246 65535; do
    for suffix in '' '/'; do
      status="storyhook daemon $version running at http://127.0.0.1:$port$suffix (PID 87092)"
      actual=$(printf '%s\n\nbackup: running at http://127.0.0.1:9999\n' "$status" | story_daemon_http_port)
      assert_eq "$?" 0 "status $version / $port$suffix: accepts a live endpoint"
      assert_eq "$actual" "$port" "status $version / $port$suffix: reads the first-line port"
    done
  done
done

for status in \
  '' \
  'storyhook daemon is not running' \
  $'storyhook daemon is not running\nbackup: running at http://127.0.0.1:55246' \
  $'\nstoryhook daemon 3.0.0 (272) running at http://127.0.0.1:55246 (PID 87092)' \
  'other daemon 3.0.0 (272) running at http://127.0.0.1:55246 (PID 87092)' \
  'storyhook daemon 3.0.0 (272) restarted at http://127.0.0.1:55246 (PID 87092)' \
  'storyhook daemon 3.0.0 (272) running at' \
  'storyhook daemon 3.0.0 (272) running at 55246'; do
  actual=$(printf '%s\n' "$status" | story_daemon_http_port)
  assert_eq "$?" 1 "invalid status: refuses [$status]"
  assert_eq "$actual" '' "invalid status: returns no endpoint [$status]"
done

for url in \
  'http://127.0.0.1:0' \
  'http://127.0.0.1:65536' \
  'http://127.0.0.1:18446744073709551615' \
  'http://127.0.0.1:-1' \
  'http://127.0.0.1:1.5' \
  'http://127.0.0.1:' \
  'http://127.0.0.1:abc' \
  'http://127.0.0.1:55246/api' \
  'http://127.0.0.1:55246?port=1' \
  'http://127.0.0.1:55246#fragment' \
  'http://127.0.0.1:55246:123' \
  'http://127.0.0.1:55246//' \
  'https://127.0.0.1:55246' \
  'http://example.com:55246' \
  'http://127.0.0.1.example.com:55246' \
  'http://127.0.0.1@evil.example:55246'; do
  actual=$(printf 'storyhook daemon 3.0.0 (272) running at %s (PID 87092)\n' "$url" | story_daemon_http_port)
  assert_eq "$?" 1 "invalid endpoint: refuses $url"
  assert_eq "$actual" '' "invalid endpoint: returns no port for $url"
done

finish
