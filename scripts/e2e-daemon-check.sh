#!/usr/bin/env bash
#
# `storyhook_daemon_is_still_ours` — after a browser run, is the daemon that
# answered it the one the harness started?
#
# WHY (SH-635). A `story daemon` is replaced silently by design: the first CLI
# call that finds the portfile's `(version, exe, exe_mtime)` is not its own
# stands the daemon down and re-spawns it, on `--port 0` a fresh port, and
# Playwright then fails every remaining test in ~150ms with ECONNREFUSED
# against the old one. 167 such failures over 25 minutes were read as 167 tree
# failures, the SH-627 shape (a dead process misread as N failing tests), and
# nothing in the output named the cause. `scripts/binary-lease.sh` removes the
# trigger; this names the shape if anything else ever produces it.
#
# WHAT IS COMPARED, AND WHAT IS NOT. Four facts, each printed by name when it
# fails: the portfile still exists; its `port` is the one the run was pointed
# at; its `exe` is the same inode as the leased binary (`-ef`, never a string
# compare -- macOS's `current_exe()` reports the invocation spelling); and the
# pid it names is alive. The PID IS DELIBERATELY NOT COMPARED against the one
# recorded at start: `e2e/specs/untrusted-origin-cookie.spec.ts` restarts the
# daemon on purpose and keeps its port (SH-321), and a check that red-flagged
# that spec would be a fixture lying to a correct gate (SH-263). The incident's
# own signature is the port moving, which the port fact catches.
#
# HOW TO USE IT:
#
#   . "$(dirname "$0")/e2e-daemon-check.sh"
#   storyhook_daemon_is_still_ours "$portfile" "$port" "$story_bin" || status=1
#
# Exit 0 when all four facts hold; otherwise a diagnosis on stderr and exit 1.
# Needs `jq`, which every caller of this file already requires.

storyhook_daemon_is_still_ours() {
  local portfile="$1" expected_port="$2" leased_bin="$3"
  local actual_port actual_exe actual_pid
  if [ ! -f "$portfile" ]; then
    echo "e2e-daemon-check: the daemon's portfile $portfile is gone -- the daemon" >&2
    echo "  this run started was stood down and nothing took its place. This is the" >&2
    echo "  machine or the harness, not the tree (SH-635)." >&2
    return 1
  fi
  actual_port="$(jq -r '.port' "$portfile" 2>/dev/null || true)"
  actual_exe="$(jq -r '.exe' "$portfile" 2>/dev/null || true)"
  actual_pid="$(jq -r '.pid' "$portfile" 2>/dev/null || true)"
  if [ "$actual_port" != "$expected_port" ]; then
    echo "e2e-daemon-check: the daemon moved from port $expected_port to port ${actual_port:-?}" >&2
    echo "  during the run. A daemon is replaced on a fresh port when a CLI call finds" >&2
    echo "  its (exe, exe_mtime) identity is not its own -- a rebuilt binary reached" >&2
    echo "  this run despite the lease, or something restarted it off-port. Every" >&2
    echo "  connection refused to :$expected_port after that point is this one event," >&2
    echo "  not a tree failure (SH-627, SH-635)." >&2
    return 1
  fi
  if [ -z "$actual_exe" ] || [ "$actual_exe" = "null" ] || ! [ "$actual_exe" -ef "$leased_bin" ]; then
    echo "e2e-daemon-check: the daemon on port $expected_port is running ${actual_exe:-an unknown executable}," >&2
    echo "  not the leased binary $leased_bin. Something started a daemon from a" >&2
    echo "  different build; its answers are not evidence about this tree (SH-635)." >&2
    return 1
  fi
  case "$actual_pid" in
    '' | null | *[!0-9]* | 0)
      echo "e2e-daemon-check: $portfile names no usable pid ('${actual_pid}')" >&2
      return 1
      ;;
  esac
  if ! kill -0 "$actual_pid" 2>/dev/null; then
    echo "e2e-daemon-check: the daemon on port $expected_port (pid $actual_pid) is dead but its" >&2
    echo "  portfile remains -- it crashed or was killed mid-run. Read the failures after" >&2
    echo "  that point as one dead daemon, not as tree failures (SH-627, SH-635)." >&2
    return 1
  fi
  return 0
}
