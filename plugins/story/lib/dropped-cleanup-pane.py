#!/usr/bin/env python3
"""Stop an abandoned pane with durable identities before every process signal."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("pane_processes", Path(__file__).with_name("stop-dispatch-pane.py"))
proc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proc)


def save(path, record):
    """Publish only a flushed complete journal; never truncate the previous evidence."""
    temporary = path.with_suffix(".tmp")
    with temporary.open("w") as stream:
        json.dump(record, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    descriptor = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def panes(target):
    """Read the exact server and all panes, including inactive and duplicate windows."""
    if not Path(target["socket"]).exists():
        return []
    command = ["tmux", "-S", target["socket"], "list-panes", "-a", "-F",
               "#{window_name}\t#{window_id}\t#{pane_id}\t#{pane_pid}"]
    output = subprocess.run(command, capture_output=True, text=True, timeout=5,
                            pass_fds=proc.inherited_fds())
    if output.returncode:
        if not output.stdout and output.stderr.strip() == f"no server running on {target['socket']}":
            return []
        raise proc.CleanupError(f"cannot inspect cleanup server: {output.stderr}")
    return [line.split("\t") for line in output.stdout.splitlines()
            if line.split("\t")[0] == target["name"]]


def require_pane(target):
    """A name cannot authorize a replacement pane, PID, or server target."""
    expected = [target["name"], target["window"], target["pane"], target["pid"]]
    if panes(target) != [expected]:
        raise proc.CleanupError("cleanup pane identity changed")
    proc.require_launch_start(int(target["pid"]), target["start"])


def signal_known(owned, sig):
    """Never send signals to a PID whose native incarnation differs."""
    table = proc.processes()
    for pid, identity in owned.items():
        if proc.same_process(table, pid, identity):
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass  # The next census proves the process has exited.


def stop(target, path):
    """Resume only the pinned process closure; replacement resources remain intact."""
    if target["pane"] == os.environ.get("TMUX_PANE"):
        raise proc.CleanupError("refusing to terminate the caller pane")
    if path.exists():
        record = json.loads(path.read_text())
        if record["target"] != target or record["phase"] not in ("census", "terminating", "complete"):
            raise proc.CleanupError("cleanup process journal identity changed")
    else:
        require_pane(target)
        record = {"target": target, "phase": "census", "owned": {}}
        save(path, record)
    owned = {int(pid): tuple(identity) for pid, identity in record["owned"].items()}
    try:
        if record["phase"] == "census":
            require_pane(target)
            signal_known(owned, signal.SIGSTOP)
            root = int(target["pid"])
            for _ in range(16):
                table = proc.processes()
                roots = {root} | {pid for pid, identity in owned.items() if proc.same_process(table, pid, identity)}
                tree = set().union(*(proc.descendants(table, pid) for pid in roots))
                known = {pid for pid, identity in owned.items() if proc.same_process(table, pid, identity)}
                new = tree - known
                if not new:
                    break
                for pid in sorted(new, key=lambda value: value != root):
                    identity = (*table[pid], proc.process_identity(pid)["start"])
                    if pid == root and identity[2] != target["start"]:
                        raise proc.CleanupError("pane process incarnation changed before freeze")
                    owned[pid] = identity
                    record["owned"] = owned
                    save(path, record)
                    # Evidence reaches disk before a signal can orphan or freeze a writer.
                    if proc.same_process(proc.processes(), pid, identity):
                        os.kill(pid, signal.SIGSTOP)
            else:
                raise proc.CleanupError("cleanup process closure did not stabilize")
            require_pane(target)
            record["phase"] = "terminating"
            save(path, record)
        if record["phase"] == "terminating":
            current = panes(target)
            if current:
                expected = [target["name"], target["window"], target["pane"], target["pid"]]
                if current != [expected]:
                    raise proc.CleanupError("replacement pane appeared during cleanup")
                # Kernel identity is checked before killing the window, as well as each PID.
                proc.require_launch_start(int(target["pid"]), target["start"])
                proc.run("tmux", "-S", target["socket"], "kill-window", "-t", target["window"])
            signal_known(owned, signal.SIGKILL)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                table = proc.processes()
                if not any(proc.same_process(table, pid, identity) for pid, identity in owned.items()):
                    break
                time.sleep(0.05)
            else:
                raise proc.CleanupError("captured cleanup writers survived termination")
            if panes(target):
                raise proc.CleanupError("story window remains after cleanup")
            record["phase"] = "complete"
            save(path, record)
        if panes(target) or any(proc.same_process(proc.processes(), pid, identity) for pid, identity in owned.items()):
            raise proc.CleanupError("completed cleanup evidence no longer proves absence")
    finally:
        # Settled errors preserve a usable session; retry must freeze a fresh closure.
        if record["phase"] == "census":
            signal_known(owned, signal.SIGCONT)


if __name__ == "__main__":
    try:
        target = json.loads(sys.argv[1])
        stop(target, Path(sys.argv[2]))
    except (proc.CleanupError, OSError, ValueError, KeyError, IndexError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"ok": False, "error": str(error)}))
        sys.exit(1)
    print(json.dumps({"ok": True, "target": target}))
