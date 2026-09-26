#!/usr/bin/env python3
"""Stop one positively identified startup pane before rolling back its worktree."""

import json
import os
import re
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True
import probe_budget
from process_identity import process_identity
from workspace_ownership import inherited_fds


class CleanupError(Exception):
    """A missing process fact that forbids destructive Git rollback."""


def run(*args):
    """Run a probe within its operation budget and preserve its diagnostic on failure."""
    result = probe_budget.run(args, capture_output=True, text=True, check=False, pass_fds=inherited_fds())
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


def alive(pid, identity):
    """Report whether a captured incarnation still runs, from its kernel identity alone.

    The native start token names exactly one incarnation, so PID reuse and
    normal reparenting need no process census. Nothing is spawned, so machine
    load cannot turn this check into a failure (SH-766). An exited or zombie
    process is gone; any other probe failure is raised.
    """
    try:
        return process_identity(pid)["start"] == identity[2]
    except ProcessLookupError:
        return False  # Exit won the race with the native probe.


def signal_known(owned, sig):
    """Signal each captured incarnation that still runs, never a recycled PID.

    Every process is tried before a failure is raised, so one unreadable
    identity cannot leave the rest of a frozen tree stopped.
    """
    failures = []
    for pid, identity in owned.items():
        try:
            if alive(pid, identity):
                os.kill(pid, sig)
        except ProcessLookupError:
            pass  # Exit won the race with our signal; callers verify absence.
        except OSError as error:
            failures.append(f"PID {pid}: {error}")
    if failures:
        raise CleanupError(f"could not send {signal.Signals(sig).name} to captured processes: {'; '.join(failures)}")


def require_launch_start(pid, expected_start):
    """Require evidence from launch, never a replacement snapshot from cleanup."""
    if not expected_start:
        raise CleanupError("missing captured launch start token; preserved process, claim and worktree")
    actual = process_identity(pid)["start"]
    if actual != expected_start:
        raise CleanupError(f"launch start changed for PID {pid}: expected {expected_start}, observed {actual}; preserved process, claim and worktree")


def stop(pane, expected_pid, expected_start):
    """Freeze, identify and terminate only this pane's startup process tree."""
    if not re.fullmatch(r"%[0-9]+", pane) or expected_pid <= 1:
        raise CleanupError("invalid captured pane/PID")
    if pane == os.environ.get("TMUX_PANE"):
        raise CleanupError("refusing to stop the caller's own pane")
    if run("tmux", "display-message", "-p", "-t", pane, "#{pane_pid}") != str(expected_pid):
        raise CleanupError("pane identity changed; preserved its process, claim and worktree")
    require_launch_start(expected_pid, expected_start)
    table = processes()
    if expected_pid not in table:
        raise CleanupError("pane process already exited; surviving descendants cannot be identified safely")
    owned = {}
    try:
        # Freeze each discovered process before re-reading ancestry. Once the
        # closure is stopped it cannot fork children between census and teardown.
        for _ in range(8):
            require_launch_start(expected_pid, expected_start)
            tree = descendants(table, expected_pid)
            new = tree - owned.keys()
            if not new:
                break
            for pid in sorted(new, key=lambda value: value != expected_pid):
                try:
                    token = process_identity(pid)["start"]
                    if pid == expected_pid and token != expected_start:
                        raise CleanupError("launch start changed before freezing its process")
                    identity = (*table[pid], token)
                    os.kill(pid, signal.SIGSTOP)
                except ProcessLookupError:
                    continue
                owned[pid] = identity
            table = processes()
        else:
            raise CleanupError("startup process tree did not stabilize before termination")
        if expected_pid not in owned or not alive(expected_pid, owned[expected_pid]):
            raise CleanupError("startup process identity changed during termination")
        if run("tmux", "display-message", "-p", "-t", pane, "#{pane_pid}") != str(expected_pid):
            raise CleanupError("pane was replaced during termination")
        require_launch_start(expected_pid, expected_start)
        run("tmux", "kill-pane", "-t", pane)
        # This is pre-charter rollback, not an agent shutdown: no task work was
        # authorized. Frozen startup children must not outlive their deleted cwd.
        signal_known(owned, signal.SIGKILL)
        # Observe before judging: a slow pass must not turn death into survival.
        while any(alive(pid, identity) for pid, identity in owned.items()):
            if probe_budget.remaining() <= 0:
                raise CleanupError("startup processes survived termination; preserved claim and worktree")
            time.sleep(0.05)
    finally:
        # A failed probe must not leave a preserved diagnostic session frozen,
        # and resuming must not depend on the census that may have failed.
        signal_known(owned, signal.SIGCONT)


if __name__ == "__main__":
    try:
        if len(sys.argv) != 4:
            raise CleanupError("expected pane, PID, and captured launch start token")
        with probe_budget.operation():
            stop(sys.argv[1], int(sys.argv[2]), sys.argv[3])
    except (CleanupError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"ok": False, "error": str(error)}))
        sys.exit(1)
    print(json.dumps({"ok": True}))
