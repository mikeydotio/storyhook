#!/usr/bin/env python3
"""Stop one positively identified startup pane before rolling back its worktree."""

import json
import os
import re
import signal
import subprocess
import sys
import time


class CleanupError(Exception):
    """A missing process fact that forbids destructive Git rollback."""


def run(*args):
    """Run a bounded probe and preserve its diagnostic on failure."""
    result = subprocess.run(args, capture_output=True, text=True, timeout=5, check=False)
    if result.returncode:
        raise CleanupError(f"{' '.join(args)}: {result.stderr.strip() or result.returncode}")
    return result.stdout.strip()


def processes():
    """Read PID, ancestry, state and start time as one process-table snapshot."""
    answer = run("ps", "-axo", "pid=,ppid=,stat=,lstart=")
    table = {}
    for line in answer.splitlines():
        fields = line.split(None, 3)
        if len(fields) != 4 or not all(value.isdecimal() for value in fields[:2]):
            raise CleanupError(f"unrecognized process-table row: {line}")
        pid, parent = map(int, fields[:2])
        if not fields[2].startswith("Z"):
            table[pid] = (parent, fields[3])
    return table


def descendants(table, root):
    """Find the transitive process tree without assuming a shared process group."""
    found = {root} if root in table else set()
    while True:
        expanded = found | {pid for pid, (parent, _) in table.items() if parent in found}
        if expanded == found:
            return found
        found = expanded


def same_process(table, pid, identity):
    """Reject PID reuse while allowing normal reparenting during termination."""
    return pid in table and table[pid][1] == identity[1]


def stop(pane, expected_pid):
    """Freeze, identify and terminate only this pane's startup process tree."""
    if not re.fullmatch(r"%[0-9]+", pane) or expected_pid <= 1:
        raise CleanupError("invalid captured pane/PID")
    if pane == os.environ.get("TMUX_PANE"):
        raise CleanupError("refusing to stop the caller's own pane")
    if run("tmux", "display-message", "-p", "-t", pane, "#{pane_pid}") != str(expected_pid):
        raise CleanupError("pane identity changed; preserved its process, claim and worktree")
    table = processes()
    if expected_pid not in table:
        raise CleanupError("pane process already exited; surviving descendants cannot be identified safely")
    owned = {}
    try:
        # Freeze each discovered process before re-reading ancestry. Once the
        # closure is stopped it cannot fork children between census and teardown.
        for _ in range(8):
            tree = descendants(table, expected_pid)
            new = tree - owned.keys()
            if not new:
                break
            for pid in sorted(new, key=lambda value: value != expected_pid):
                identity = table[pid]
                try:
                    os.kill(pid, signal.SIGSTOP)
                except ProcessLookupError:
                    continue
                owned[pid] = identity
            table = processes()
        else:
            raise CleanupError("startup process tree did not stabilize before termination")
        if expected_pid not in owned or not same_process(table, expected_pid, owned[expected_pid]):
            raise CleanupError("startup process identity changed during termination")
        if run("tmux", "display-message", "-p", "-t", pane, "#{pane_pid}") != str(expected_pid):
            raise CleanupError("pane was replaced during termination")
        run("tmux", "kill-pane", "-t", pane)
        # This is pre-charter rollback, not an agent shutdown: no task work was
        # authorized. Frozen startup children must not outlive their deleted cwd.
        table = processes()
        for pid, identity in owned.items():
            if same_process(table, pid, identity):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass  # Exit won the race with our signal; verified below.
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            table = processes()
            if not any(same_process(table, pid, identity) for pid, identity in owned.items()):
                return
            time.sleep(0.05)
        raise CleanupError("startup processes survived termination; preserved claim and worktree")
    finally:
        # A failed tmux call must not leave a preserved diagnostic session frozen.
        table = processes()
        for pid, identity in owned.items():
            if same_process(table, pid, identity):
                try:
                    os.kill(pid, signal.SIGCONT)
                except ProcessLookupError:
                    pass


if __name__ == "__main__":
    try:
        stop(sys.argv[1], int(sys.argv[2]))
    except (CleanupError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"ok": False, "error": str(error)}))
        sys.exit(1)
    print(json.dumps({"ok": True}))
