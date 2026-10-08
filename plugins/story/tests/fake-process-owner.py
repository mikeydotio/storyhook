#!/usr/bin/env python3
"""Native ownership for plugin fake panes; mutable tmux PID fields grant nothing."""
import contextlib
import fcntl
import json
import math
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts"))
from host_admission import native
from host_admission.policy import Refusal as NativeRefusal


class Refusal(Exception):
    pass


def check_deadline(deadline):
    if time.monotonic() >= deadline:
        raise Refusal("fixture operation deadline expired; retain roots")


def atomic(path, value):
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex)
    temporary.write_text(json.dumps(value))
    temporary.replace(path)


def identity(pid):
    return native.identity(pid, native.boot_identity())


def live(owner):
    try:
        current = identity(owner["pid"])
    except ProcessLookupError:
        return False
    if current != owner:
        raise Refusal("recorded PID has a different native incarnation; no signal allowed")
    return True


def file_pin(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise Refusal("fixture custody file is not an original regular file")
    return [info.st_dev, info.st_ino]


def pinned(path, expected):
    if file_pin(path) != expected:
        raise Refusal("fixture custody file identity changed; retain roots")


def pinned_descriptor(path, expected, descriptor):
    pinned(path, expected)
    info = os.fstat(descriptor)
    if not stat.S_ISREG(info.st_mode) or [info.st_dev, info.st_ino] != expected:
        raise Refusal("fixture custody descriptor identity changed")


def original(root):
    stamp = json.loads((root / "owner.json").read_text())
    info = root.stat()
    if root.is_symlink() or (info.st_dev, info.st_ino) != tuple(stamp["directory"]):
        raise Refusal("fixture process ledger directory changed")
    for name in ("lock", "writers"):
        pinned(root / name, stamp["files"][name])
    return stamp


@contextlib.contextmanager
def locked(root, deadline):
    stamp = original(root)
    with (root / "lock").open("r+") as lock:
        pinned_descriptor(root / "lock", stamp["files"]["lock"], lock.fileno())
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise Refusal("fixture process admission lock did not settle")
                time.sleep(0.01)
        original(root)
        pinned_descriptor(root / "lock", stamp["files"]["lock"], lock.fileno())
        yield lock


def require_open(root):
    if (root / "admission").read_text() != "open":
        raise Refusal("fixture process admission is closed")
    if not live(original(root)["owner"]):
        raise Refusal("fixture process owner exited")


def scope_path(root, scope):
    if len(scope) != 32 or any(c not in "0123456789abcdef" for c in scope):
        raise Refusal("invalid fixture scope")
    return root / (scope + ".scope.json")


def scope_record(root, scope):
    path = scope_path(root, scope)
    file_pin(path)
    row = json.loads(path.read_text())
    if row["ledger"] != original(root)["nonce"]:
        raise Refusal("scope belongs to a different fixture ledger")
    pinned(root / (scope + ".scope-writers"), row["writers"])
    return row


def open_scope(root, scope):
    row = scope_record(root, scope)
    if row["closed"] or not live(row["owner"]):
        raise Refusal("fixture scope no longer admits writers")
    return row


def begin_scope(root, owner_pid, deadline):
    with locked(root, deadline):
        require_open(root)
        scope = uuid.uuid4().hex
        (root / (scope + ".scope-writers")).touch()
        atomic(scope_path(root, scope), dict(owner=identity(owner_pid), closed=False,
                                            ledger=original(root)["nonce"],
                                            writers=file_pin(root / (scope + ".scope-writers"))))
        return scope


def admit_writer(root, scope, descriptor, scope_descriptor, deadline):
    with locked(root, deadline):
        require_open(root)
        scope_row = open_scope(root, scope)
        for fd, path, expected in (
                (descriptor, root / "writers", original(root)["files"]["writers"]),
                (scope_descriptor, root / (scope + ".scope-writers"), scope_row["writers"])):
            pinned_descriptor(path, expected, fd)
            fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
            pinned_descriptor(path, expected, fd)


def entries(root):
    paths = sorted(root.glob("*.process.json"))
    if len(paths) > 4096:
        raise Refusal("fixture process ledger exceeds its bound")
    result = []
    for path in paths:
        file_pin(path)
        record = json.loads(path.read_text())
        validate_record(root, path, record)
        result.append((path, record))
    return result


def validate_record(root, path, record):
    stamp = original(root)
    if record["ledger"] != stamp["nonce"] or record["directory"] != stamp["directory"]:
        raise Refusal("launch record belongs to a different fixture ledger")
    scope_record(root, record["scope"])
    name = path.name.removesuffix(".process.json")
    child = name.endswith(".child")
    launch = name.removesuffix(".child")
    if len(launch) != 32 or any(c not in "0123456789abcdef" for c in launch):
        raise Refusal("invalid launch record filename")
    if (record["role"] == "child") != child or record["role"] not in ("child", "pane", "pane-child", "publisher"):
        raise Refusal("invalid launch record role")
    if record["writer"] != launch + ".writer":
        raise Refusal("launch writer is not its exact local basename")
    pinned(root / record["writer"], record["writer_pin"])
    if child:
        parent = json.loads((root / (launch + ".process.json")).read_text())
        if parent["role"] != "pane-child" or any(
                parent[key] != record[key] for key in ("ledger", "directory", "scope", "state", "session", "writer", "writer_pin")):
            raise Refusal("child is not bound to its registered pane launch")


def exclusive(path, expected, deadline):
    pinned(path, expected)
    stream = path.open("r")
    try:
        pinned_descriptor(path, expected, stream.fileno())
        while True:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                pinned_descriptor(path, expected, stream.fileno())
                return stream
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise Refusal("fixture writer still borrows its state; retain roots")
                time.sleep(0.01)
    except BaseException:
        stream.close()
        raise


def stop_entries(root, selected, deadline):
    # Native identity mismatch refuses before any PID receives a signal.
    for path, record in selected:
        validate_record(root, path, record)
        if not record.get("settled"):
            live(record["native"])
    for child_phase in (True, False):
        phase = [(path, row) for path, row in selected
                 if (row["role"] == "child") == child_phase and not row.get("settled")]
        for _, row in phase:
            if live(row["native"]):
                check_deadline(deadline)
                try:
                    os.kill(row["native"]["pid"], signal.SIGTERM)
                except ProcessLookupError:
                    pass
        term_until = min(deadline, time.monotonic() + 1)
        for path, row in phase:
            while live(row["native"]):
                if time.monotonic() >= deadline:
                    raise Refusal("fixture process did not settle; retain roots")
                if time.monotonic() >= term_until:
                    # Recheck immediately before this exact-PID signal too.
                    if live(row["native"]):
                        try:
                            os.kill(row["native"]["pid"], signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                time.sleep(0.01)
            if row["role"] != "child":
                with exclusive(root / row["writer"], row["writer_pin"], deadline):
                    pass
            row["settled"] = True
            atomic(path, row)


def retire(root, scope, state, deadline, session=None):
    with locked(root, deadline):
        require_open(root)
        open_scope(root, scope)
        selected = [(path, row) for path, row in entries(root)
                    if row["scope"] == scope and row["state"] == str(state.resolve())
                    and (session is None or row["session"] == session)]
        stop_entries(root, selected, deadline)


def cleanup_scope(root, scope, owner_pid, deadline):
    with locked(root, deadline):
        path = scope_path(root, scope)
        row = scope_record(root, scope)
        if identity(owner_pid) != row["owner"]:
            raise Refusal("only the original scope shell may retire its writers")
        row["closed"] = True
        atomic(path, row)
        stop_entries(root, [(p, r) for p, r in entries(root) if r["scope"] == scope], deadline)
    with exclusive(root / (scope + ".scope-writers"), row["writers"], deadline):
        pass


def cleanup(root, owner_pid, deadline):
    with locked(root, deadline):
        stamp = original(root)
        if identity(owner_pid) != stamp["owner"]:
            raise Refusal("only the original fixture shell may clean its process ledger")
        # A nested shell is its own owner, not a signal target. Its scope must
        # finish before an outer EXIT can delete shared fixture roots.
        for path in root.glob("*.scope.json"):
            scope = path.name.removesuffix(".scope.json")
            row = scope_record(root, scope)
            if not row["closed"] and live(row["owner"]):
                raise Refusal("nested fixture scope is still active; retain roots")
        (root / "admission").write_text("closed")
        stop_entries(root, entries(root), deadline)
    # In-flight tmux invocations and all inherited publishers must finish too.
    with exclusive(root / "writers", stamp["files"]["writers"], deadline):
        pass


def start(root, scope, state, kind, lifetime, session, extra, deadline):
    name = uuid.uuid4().hex
    record_path = root / (name + ".process.json")
    writer_path = root / (name + ".writer")
    gate_read, gate_write = os.pipe()
    ready_read, ready_write = os.pipe()
    child = None
    try:
        with writer_path.open("x+") as writer:
            fcntl.flock(writer, fcntl.LOCK_SH)
            with locked(root, deadline):
                require_open(root)
                open_scope(root, scope)
                check_deadline(deadline)
                child = subprocess.Popen(
                    [sys.executable, "-B", __file__, "worker", str(root), str(state),
                     name, kind, lifetime, session, str(gate_read), str(ready_write),
                     str(writer.fileno()), scope, str(deadline), *extra], stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    pass_fds=(7, 8, gate_read, ready_write, writer.fileno()))
                stamp = original(root)
                record = dict(native=identity(child.pid), role=kind, state=str(state.resolve()),
                              session=session, scope=scope, writer=writer_path.name, settled=False,
                              writer_pin=file_pin(writer_path), ledger=stamp["nonce"], directory=stamp["directory"])
                atomic(record_path, record)
                os.write(gate_write, b"G")
            os.close(gate_read)
            gate_read = -1
            os.close(ready_write)
            ready_write = -1
            os.set_blocking(ready_read, False)
            while True:
                try:
                    answer = os.read(ready_read, 1)
                except BlockingIOError:
                    answer = None
                if answer == b"R":
                    return child.pid
                if answer == b"" or time.monotonic() >= deadline:
                    raise Refusal("fixture worker did not publish registered readiness")
                time.sleep(0.01)
    except BaseException:
        # A registered worker remains in the durable ledger. An unregistered
        # direct child is still held here and cannot have passed its gate.
        if child is not None and not record_path.exists():
            child.kill()
            child.wait(timeout=max(0.001, deadline - time.monotonic()))
        raise
    finally:
        for descriptor in (gate_read, gate_write, ready_read, ready_write):
            if descriptor >= 0:
                os.close(descriptor)


def worker(args):
    root, state = Path(args[0]), Path(args[1])
    name, kind, lifetime, session = args[2:6]
    gate, ready, writer = map(int, args[6:9])
    scope, deadline = args[9], float(args[10])
    if os.read(gate, 1) != b"G":
        return
    os.close(gate)
    os.set_inheritable(writer, True)
    os.set_inheritable(8, True)
    os.set_inheritable(7, True)
    if kind == "pane-child":
        child = None
        try:
            with locked(root, deadline):
                require_open(root)
                open_scope(root, scope)
                check_deadline(deadline)
                child = subprocess.Popen(["sleep", lifetime], pass_fds=(7, 8, writer))
                parent = json.loads((root / (name + ".process.json")).read_text())
                record = dict(parent, native=identity(child.pid), role="child", settled=False)
                atomic(root / (name + ".child.process.json"), record)
                (state / "child_pid").write_text(str(child.pid))
            os.write(ready, b"R")
            os.close(ready)
            child.wait()
        except BaseException:
            if child is not None:
                child.kill()
                child.wait(timeout=max(0.001, deadline - time.monotonic()))
            raise
    elif kind == "pane":
        os.write(ready, b"R")
        os.close(ready)
        os.execvp("sleep", ["sleep", lifetime])
    elif kind == "publisher":
        os.write(ready, b"R")
        os.close(ready)
        time.sleep(float(lifetime))
        os.execv("/bin/bash", ["bash", args[11], "--publish-owned-hook", args[12]])
    else:
        raise Refusal("unknown fixture worker role")


def main(argv):
    action, root = argv[1], Path(argv[2])
    allowance = float(os.environ.get("FAKE_TMUX_CLEANUP_SECONDS", "30"))
    if not math.isfinite(allowance) or allowance <= 0 or allowance > 900:
        raise Refusal("fixture custody allowance must be within (0, 900] seconds")
    deadline = time.monotonic() + allowance
    if action == "init":
        info = root.stat()
        for name in ("lock", "writers"):
            with (root / name).open("x"):
                pass
        atomic(root / "owner.json", dict(owner=identity(int(argv[3])), nonce=uuid.uuid4().hex,
                                        directory=[info.st_dev, info.st_ino],
                                        files={name: file_pin(root / name) for name in ("lock", "writers")}))
        (root / "admission").write_text("open")
    elif action == "scope":
        print(begin_scope(root, int(argv[3]), deadline))
    elif action == "writer":
        admit_writer(root, argv[3], int(argv[4]), int(argv[5]), deadline)
    elif action == "start":
        print(start(root, os.environ["FAKE_TMUX_PROCESS_SCOPE"], Path(argv[3]), argv[4], argv[5], argv[6], argv[7:], deadline))
    elif action == "retire":
        retire(root, os.environ["FAKE_TMUX_PROCESS_SCOPE"], Path(argv[3]), deadline, argv[4] if len(argv) > 4 else None)
    elif action == "cleanup-scope":
        cleanup_scope(root, argv[3], int(argv[4]), deadline)
    elif action == "cleanup":
        cleanup(root, int(argv[3]), deadline)
    elif action == "worker":
        worker(argv[2:])
    else:
        raise Refusal("unknown fixture owner action")


if __name__ == "__main__":
    try:
        main(sys.argv)
    except (OSError, ValueError, KeyError, Refusal, NativeRefusal) as error:
        print(f"fake process custody refused: {error}", file=sys.stderr)
        raise SystemExit(1)
