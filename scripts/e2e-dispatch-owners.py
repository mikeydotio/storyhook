#!/usr/bin/env python3
"""Own and drain daemon-created E2E helpers before deleting a slice (SH-807).

Registration precedes every fake-tmux access. A wrapper remains its group's
leader until the real helper returns, then captures surviving placeholders.
Cleanup never grants signal authority from a historical bare PID.
"""

import contextlib
import fcntl
import json
import os
from pathlib import Path
import signal
import sys
import time

sys.dont_write_bytecode = True
from host_admission import native


class UnsafeCleanup(Exception):
    """Preserve the fixture when ownership or quiescence cannot be proved."""


def observed(pid):
    try:
        value = native.process(pid, "fixture")
        return value if value["live"] else None
    except ProcessLookupError:
        return None


def same(owner):
    value = observed(owner["pid"])
    return value is not None and value["start"] == owner["start"]


@contextlib.contextmanager
def locked(root, deadline):
    # Never create here: late registration cannot recreate a removed root.
    with (root / "lock").open("r+") as stream:
        while True:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise UnsafeCleanup("helper ownership lock did not settle")
                time.sleep(0.01)
        stream.seek(0)
        yield stream


def initialize(root):
    root.mkdir(mode=0o700)
    (root / "lock").touch(mode=0o600)
    (root / "lock").write_text("open")


def register(root, pid, deadline):
    with locked(root, deadline) as stream:
        if stream.read() != "open":
            raise UnsafeCleanup("slice cleanup has closed helper admission")
        owner = observed(pid)
        if owner is None or os.getpgid(pid) != pid:
            raise UnsafeCleanup("helper is not an isolated live process-group leader")
        (root / f"{pid}.json").write_text(json.dumps({"owner": owner}))


def group_members(group):
    found = {}
    for pid in native.pids():
        try:
            if os.getpgid(pid) == group:
                value = observed(pid)
                if value is not None:
                    found[pid] = value
        except ProcessLookupError:
            continue
    return found


def complete(root, pid, deadline):
    with locked(root, deadline):
        path = root / f"{pid}.json"
        receipt = json.loads(path.read_text())
        owner = receipt["owner"]
        if not same(owner) or os.getpgid(pid) != pid:
            raise UnsafeCleanup("helper incarnation changed before completion")
        # Placeholders deliberately outlive successful dispatch. Capture them
        # while the launch leader still proves the group, not later via pane_pid.
        receipt["completed"] = list(group_members(pid).values())
        if not same(owner):
            raise UnsafeCleanup("helper exited during completion observation")
        path.write_text(json.dumps(receipt))


def close(root, deadline):
    with locked(root, deadline) as stream:
        # Persist closure in the locked inode, not a separate removable path.
        # A late waiter holding an already-unlinked descriptor still sees it.
        stream.write("closed")
        stream.truncate()
        stream.flush()


def signal_known(owned, sig):
    failures = []
    for owner in owned.values():
        try:
            if same(owner):
                os.kill(owner["pid"], sig)
        except ProcessLookupError:
            pass
        except OSError as error:
            failures.append(str(error))
    if failures:
        raise UnsafeCleanup("; ".join(failures))


def drain_receipt(receipt, deadline):
    owner = receipt["owner"]
    group = owner["pid"]
    if type(group) is not int or group <= 1 or group == os.getpgrp():
        raise UnsafeCleanup("invalid or caller-owned helper group")
    current = observed(group)
    if current is not None and current["start"] != owner["start"]:
        raise UnsafeCleanup(f"helper {group} incarnation changed")
    if "completed" in receipt:
        owned = {value["pid"]: value for value in receipt["completed"]}
        # Completed helpers can leave placeholders but cannot admit new writers.
        # Refuse, rather than adopt, a group member absent from the saved census.
        for pid, value in group_members(group).items():
            if pid not in owned or owned[pid]["start"] != value["start"]:
                raise UnsafeCleanup("completed helper group has an unrecorded process")
    elif current is None:
        if group_members(group):
            raise UnsafeCleanup(f"helper {group} exited without a completion receipt; group remains")
        return
    else:
        if os.getpgid(group) != group:
            raise UnsafeCleanup("helper process group changed")
        owned = {group: current}
    try:
        # Freeze the proven leader first, then freeze the group to a fixpoint.
        # A writer cannot fork after the final census and escape deletion's barrier.
        signal_known(owned, signal.SIGSTOP)
        if "completed" not in receipt:
            while True:
                if not same(owner):
                    raise UnsafeCleanup("helper leader disappeared during capture")
                captured = group_members(group)
                if any(pid in owned and owned[pid]["start"] != value["start"]
                       for pid, value in captured.items()):
                    raise UnsafeCleanup("helper member incarnation changed during capture")
                new = {pid: value for pid, value in captured.items() if pid not in owned}
                if not new:
                    break
                owned.update(new)
                signal_known(new, signal.SIGSTOP)
                if time.monotonic() >= deadline:
                    raise UnsafeCleanup("helper group did not stabilize")
        else:
            # A placeholder could fork between the saved census and SIGSTOP.
            # Without its leader we cannot adopt more members; retain evidence.
            for pid, value in group_members(group).items():
                if pid not in owned or owned[pid]["start"] != value["start"]:
                    raise UnsafeCleanup("completed helper group changed during capture")
        # Synthetic fixture only: do not run writer EXIT traps during removal.
        signal_known(owned, signal.SIGKILL)
        while any(same(value) for value in owned.values()):
            if time.monotonic() >= deadline:
                raise UnsafeCleanup("helper writer survived termination")
            time.sleep(0.01)
        if group_members(group):
            raise UnsafeCleanup("unrecorded helper group members survived termination")
    finally:
        # A failed observation must not strand preserved processes frozen.
        signal_known(owned, signal.SIGCONT)


def drain(root, deadline):
    with locked(root, deadline) as stream:
        if stream.read() != "closed":
            raise UnsafeCleanup("helper admission must close before draining")
        for path in sorted(root.glob("*.json")):
            drain_receipt(json.loads(path.read_text()), deadline)
            path.unlink()


def main(argv):
    if len(argv) < 3:
        raise UnsafeCleanup("expected init|register|complete|close|drain OWNER_DIR [PID]")
    action, root = argv[1], Path(argv[2])
    # Fixture teardown is bounded by the pool's existing ten-second kill grace.
    deadline = time.monotonic() + 5
    if action == "init":
        initialize(root)
    elif action in ("register", "complete") and len(argv) == 4:
        globals()[action](root, int(argv[3]), deadline)
    elif action in ("close", "drain") and len(argv) == 3:
        globals()[action](root, deadline)
    else:
        raise UnsafeCleanup("invalid helper ownership command")


if __name__ == "__main__":
    try:
        main(sys.argv)
    except (OSError, ValueError, KeyError, RuntimeError, UnsafeCleanup) as error:
        print(f"e2e dispatch cleanup: {error}; preserve the slice root", file=sys.stderr)
        sys.exit(1)
