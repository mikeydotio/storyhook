"""Whole-build custody, independent of host resource admission (SH-835).

The permanent product lock lives in the checkout's *private* Git directory.
Every launch is journalled before execution. A lost supervisor never becomes
permission to reclaim: its unfinished journal survives even if all FDs close.
This module does not enroll legacy worktrees or delete build products.
"""

import fcntl
import json
import os
from pathlib import Path
import stat
import subprocess
import time
import uuid

from host_admission import native
from host_admission.namespace import directory, open_private
from host_admission.policy import Refusal
from host_admission.supervisor import ManagedProcess

PRODUCT_FD = "STORYHOOK_PRODUCT_LEASE_FD"
PRODUCT_ROOT = "STORYHOOK_PRODUCT_LEASE_ROOT"


def git_path(cwd, option):
    result = subprocess.run(["git", "-C", str(cwd), "rev-parse", option],
                            check=True, text=True, capture_output=True, timeout=10)
    return Path(result.stdout.strip()).resolve(strict=True)


def namespace(cwd):
    """Resolve Git identity before opening any ownership file."""
    private = git_path(cwd, "--absolute-git-dir")
    return directory(private / "storyhook-build-products-v1", create=True)


def write_record(path, value, *, create=False):
    """Publish an fsynced record; new owners cannot replace an older owner."""
    temporary = path.with_name(".record-" + uuid.uuid4().hex)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(value, stream, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        if create:
            os.link(temporary, path)
            temporary.unlink()
        else:
            os.replace(temporary, path)
        parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
    finally:
        temporary.unlink(missing_ok=True)


def read_record(path):
    fd = open_private(path)
    with os.fdopen(fd) as stream:
        try:
            value = json.load(stream)
        except (ValueError, UnicodeError) as error:
            raise Refusal(f"unreadable product owner {path}: {error}") from error
    if not isinstance(value, dict) or value.get("version") != 1:
        raise Refusal(f"unknown product owner record: {path}")
    return value


class ProductLease:
    """A shared whole-build lease, or an exclusive reclaim reservation.

    Never explicitly unlock: descendants inherit the same open file description.
    Never unlink/replace the permanent lock inode. Reclamation is nonblocking.
    Managed builds may wait up to 30 seconds for a short detach operation, before
    launching or reserving an owner; cancellation cannot strand a build record.
    Neither waiting nor refusal guesses ownership from a PID or pane snapshot.
    """

    def __init__(self, root, *, reclaim=False, wait_seconds=0):
        self.root = directory(root)
        self.fd = open_private(self.root / "products.lock", create=True)
        try:
            mode = fcntl.LOCK_EX if reclaim else fcntl.LOCK_SH
            deadline = time.monotonic() + wait_seconds
            while True:
                try:
                    fcntl.flock(self.fd, mode | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    if reclaim or not wait_seconds:
                        raise
                    if time.monotonic() >= deadline:
                        raise Refusal("managed build timed out waiting for short product detachment") from None
                    # No build has launched and no journal was reserved yet.
                    # SIGINT/TERM can cancel this wait without stranding an owner.
                    time.sleep(min(0.05, max(0, deadline - time.monotonic())))
            if reclaim:
                self.require_settled()
            os.set_inheritable(self.fd, True)
        except BaseException:
            os.close(self.fd)
            self.fd = None
            raise

    def require_settled(self):
        """Crash/unknown evidence is preserved, even when no process is visible."""
        for path in self.root.iterdir():
            if path.name == "products.lock":
                continue
            if not path.name.startswith("build-"):
                raise Refusal(f"unknown product custody entry: {path}")
            directory(path)
            row = read_record(path / "record.json")
            if row.get("state") != "finished" or row.get("executions"):
                raise Refusal(f"unsettled product owner: {path.name}")

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def inherited_lease(root, env):
    """Only a held descriptor for this permanent inode permits a nested exec."""
    fd_text, claimed = env.get(PRODUCT_FD), env.get(PRODUCT_ROOT)
    if fd_text is None and claimed is None:
        return None
    if claimed is not None and claimed != str(root):
        # A managed caller may build a different Git checkout. It needs its
        # own custody while the inherited outer descriptor remains open.
        return None
    if not fd_text or not fd_text.isdecimal():
        raise Refusal("inherited product lease does not identify this worktree")
    fd = int(fd_text)
    try:
        actual = os.fstat(fd)
        expected = (root / "products.lock").lstat()
        if (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino):
            raise Refusal("inherited product lease inode changed")
        if not stat.S_ISREG(actual.st_mode) or not os.get_inheritable(fd):
            raise Refusal("inherited product lease is not an inheritable regular file")
        probe = open_private(root / "products.lock")
        try:
            try:
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return fd
            raise Refusal("inherited product lease is not held")
        finally:
            os.close(probe)
    except OSError as error:
        raise Refusal(f"invalid inherited product lease: {error}") from error


class ProductCustody:
    """Durable local custody adapter for ManagedProcess; never a host grant.

    The sole writer is the supervisor. Reclaim reads only while exclusively
    holding the product lock, so it cannot race a live writer. Unfinished records
    require explicit recovery; neither age nor an absent process erases them.
    """

    def __init__(self, root, command):
        token = uuid.uuid4().hex
        self.root = Path(root) / ("build-" + token)
        self.root.mkdir(mode=0o700)
        self.lease = {"id": token, "token": token}
        self.row = dict(version=1, **self.lease, state="reserved", executions=[],
                        command=command, owner=native.identity(os.getpid(), native.boot_identity()))
        write_record(self.root / "record.json", self.row, create=True)

    def call(self, operation, **arguments):
        if operation == "status":
            # Custody deadlines, not workload CPU/RSS estimates. The launch
            # handshake gets 10 s; draining gets TERM then KILL with 5 s each.
            return {"timing": {"lease_ms": 10000, "cleanup_ms": 5000, "sample_ms": 100}}
        if any(arguments.get(key) != self.lease[key] for key in ("id", "token")):
            raise Refusal("product custody identity mismatch")
        if operation == "inspect":
            return self.row.copy()
        if operation == "attach":
            if self.row["state"] != "reserved" or self.row["executions"]:
                raise Refusal("product custody already attached")
            self.row["executions"] = [arguments["execution"]]
            self.row["state"] = "running"
        elif operation == "cancel":
            self.row["state"] = "draining"
        elif operation == "settle":
            execution, = self.row["executions"]
            if execution["id"] != arguments["execution_id"]:
                raise Refusal("product execution identity mismatch")
            # A child that leaves the original session still holds the guard.
            # If it closes all custody FDs, that is outside the managed contract.
            guard = open_private(self.root / execution["guard"])
            try:
                fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
                if native.session_members(execution["session"]):
                    raise Refusal("product build session has surviving processes")
            except BlockingIOError:
                raise Refusal("product build lifetime guard is still held") from None
            finally:
                os.close(guard)
            self.row["settled_execution"] = execution
            self.row["executions"] = []
        elif operation == "finish":
            if self.row["executions"]:
                return False
            self.row["state"] = "finished"
        else:
            raise Refusal(f"unknown product custody operation: {operation}")
        write_record(self.root / "record.json", self.row)
        return True


def run_managed(command, *, cwd=None, env=None):
    """Own every Cargo phase, including cached build scripts and descendants."""
    root = namespace(Path.cwd() if cwd is None else cwd)
    env = dict(os.environ if env is None else env)
    if inherited_lease(root, env) is not None:
        os.execvpe(command[0], command, env)
    with ProductLease(root, wait_seconds=30) as lease:
        env.update({PRODUCT_FD: str(lease.fd), PRODUCT_ROOT: str(root)})
        custody = ProductCustody(root, command)
        process = ManagedProcess(custody, custody.lease, command, env=env,
                                 grant_environment=False, cwd=cwd, forward_signals=True)
        try:
            result = process.wait()
            return result
        finally:
            process.close()
