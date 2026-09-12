#!/usr/bin/env python3
"""Native turn interruption behind notify's provider gate, with owned-gate cleanup."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import time

sys.dont_write_bytecode = True

# Share the startup rollback's bounded probes and PID/start identity rules.
# Its destructive stop operation is deliberately not used for an agent session.
spec = importlib.util.spec_from_file_location("pane_processes", Path(__file__).with_name("stop-dispatch-pane.py"))
proc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proc)


def cancelled(signum, _frame):
    """Run the ordinary failure cleanup when the daemon cancels its helper."""
    raise proc.CleanupError(f"interruption controller received signal {signum}; cleanup uncertain")


signal.signal(signal.SIGTERM, cancelled)
signal.signal(signal.SIGHUP, cancelled)


def processes():
    """Normalize ps padding to the lock owner's canonical start-time spelling."""
    return {pid: (parent, " ".join(start.split()))
            for pid, (parent, start) in proc.processes().items()}


def target(pane, provider):
    """Bind a delivery to a server, pane and exec-launched provider lifetime."""
    if provider not in ("codex", "claude"):
        raise proc.CleanupError("unrecognized provider")
    pid = int(proc.run("tmux", "display-message", "-p", "-t", pane, "#{pane_pid}"))
    table = processes()
    if pid <= 1 or pid not in table:
        raise proc.CleanupError("pane process exited before identity capture")
    if proc.run("tmux", "show-options", "-w", "-v", "-t", pane, "@storyhook-agent") != provider:
        raise proc.CleanupError("pane provider changed during capture")
    return json.dumps([proc.run("tmux", "display-message", "-p", "-t", pane, "#{socket_path}"),
                       pane, pid, table[pid][1], provider], separators=(",", ":"))


def signal_known(owned, sig):
    """Signal only still-live captured identities, never recycled PIDs."""
    table = processes()
    for pid, identity in owned.items():
        if proc.same_process(table, pid, identity):
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass  # Rechecked for quiescence by the caller.


def freeze(owned, roots):
    """Close the fork race before native cancellation can orphan a gate tree."""
    for _ in range(16):
        table = processes()
        live_roots = {pid for pid, identity in {**roots, **owned}.items()
                      if proc.same_process(table, pid, identity)}
        tree = set().union(*(proc.descendants(table, pid) for pid in live_roots))
        new = tree - owned.keys()
        if not new:
            return
        for pid in new:
            identity = table[pid]
            try:
                os.kill(pid, signal.SIGSTOP)
                owned[pid] = identity
            except ProcessLookupError:
                pass
    raise proc.CleanupError("owned gate process tree did not stabilize")


def interrupt(pane, provider, expected):
    """Send only Escape, then prove captured gate writers are gone before release."""
    if target(pane, provider) != expected:
        raise proc.CleanupError("pane identity changed before interruption")
    root = json.loads(expected)[2]
    table = processes()
    tree = proc.descendants(table, root)
    lock_root = Path(os.environ.get("STORYHOOK_LOCK_DIR", str(Path.home() / ".local/state/storyhook/locks")))
    holders, guards, owned = {}, [], {}
    try:
        for lock in sorted(lock_root.glob("gate*.lock")):
            try:
                pid = int((lock / "pid").read_text().strip())
                started = (lock / "started").read_text().strip()
            except FileNotFoundError:
                continue  # A concurrent normal release needs no cancellation.
            if pid == root or pid not in tree or table[pid][1] != started:
                continue
            # Old gate implementations cannot promise to honor our ownership guard.
            if not (lock / "interrupt-protocol").exists():
                raise proc.CleanupError(f"gate {lock} predates interruption-safe ownership; no native key sent")
            holders[pid] = table[pid]
            freeze(owned, {pid: table[pid]})
            if not proc.same_process(processes(), pid, table[pid]):
                raise proc.CleanupError(f"gate owner {pid} exited during capture; cleanup uncertain")
            if (lock / "pid").read_text().strip() != str(pid) or (lock / "started").read_text().strip() != started:
                raise proc.CleanupError(f"gate {lock} changed owner during capture")
            guard = lock / "interrupt"
            guard.mkdir()
            guards.append((guard, pid, started))
            (guard / "started").write_text(processes()[os.getpid()][1] + "\n")
            (guard / "owner").write_text(f"{os.getpid()}\n")
        if target(pane, provider) != expected:
            raise proc.CleanupError("pane was replaced while capturing gates")
        for guard, _, _ in guards:
            (guard / "processes.json").write_text(json.dumps({"target": expected, "processes": owned}))
        # Both supported provider TUIs use Escape for a full turn interrupt.
        # No prompt text, submit key, exit command, claim or worktree operation.
        proc.run("tmux", "send-keys", "-t", pane, "Escape")
        signal_known(holders, signal.SIGTERM)
        signal_known(owned, signal.SIGCONT)
        # The lock owner's existing TERM trap does normal group cleanup first.
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            table = processes()
            if not any(proc.same_process(table, pid, identity) for pid, identity in owned.items()):
                break
            time.sleep(.05)
        else:
            # Native interruption may have killed the wrapper before its trap.
            # Freeze the surviving captured closure, terminate exactly that tree,
            # and retain the guard until a fresh process census proves quiescence.
            freeze(owned, {})
            signal_known(owned, signal.SIGKILL)
        deadline = time.monotonic() + 5
        while True:
            table = processes()
            if not any(proc.same_process(table, pid, identity) for pid, identity in owned.items()):
                break
            if time.monotonic() >= deadline:
                raise proc.CleanupError("gate children survived cancellation; ownership guard retained")
            time.sleep(.05)
        for guard, pid, started in guards:
            lock = guard.parent
            if ((lock / "pid").read_text().strip() != str(pid)
                    or (lock / "started").read_text().strip() != started
                    or (guard / "owner").read_text().strip() != str(os.getpid())):
                raise proc.CleanupError(f"gate {lock} identity changed before release")
            # The preserved provider may not reap its zombie shell immediately.
            # Proven quiescence, not kill -0, authorizes this exact lock release.
            retired = lock.with_name(f"{lock.name}.interrupted-{os.getpid()}")
            lock.rename(retired)
            shutil.rmtree(retired)
        return expected
    finally:
        # Failed probes preserve an inspectable session and visible ownership guard.
        signal_known(owned, signal.SIGCONT)


if __name__ == "__main__":
    try:
        operation, pane, provider = sys.argv[1:4]
        if operation == "target":
            print(target(pane, provider))
        elif operation == "interrupt":
            print(interrupt(pane, provider, sys.argv[4]))
        else:
            raise proc.CleanupError(f"unknown operation: {operation}")
    except (proc.CleanupError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
