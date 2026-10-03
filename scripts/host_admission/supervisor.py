"""Managed launches retain their budget until session and lifetime guard settle."""

import fcntl
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time
import uuid

from . import native
from .namespace import open_private
from .policy import Refusal


class ManagedProcess:
    """A blocked-launch supervisor; SH-869 adapters own preserving its child contract."""

    def __init__(self, client, lease, command):
        self.client, self.lease, self.child, self.guard = client, lease, None, None
        self.finished = False
        self.execution_id = uuid.uuid4().hex
        self.boot = native.boot_identity()
        current = self.call("inspect")
        if current["state"] != "reserved":
            raise Refusal("managed launch requires a reserved grant")
        self.timing = client.call("status")["timing"]
        if not command or not all(isinstance(arg, str) and "\0" not in arg for arg in command):
            raise Refusal("managed command must be a nonempty argv")
        guard_name = f"lease-{lease['token']}.lock"
        self.guard = open_private(client.root / guard_name, create=True)
        fcntl.flock(self.guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        read_go, write_go = os.pipe()
        read_ready, write_ready = os.pipe()
        try:
            env = dict(os.environ, STORYHOOK_HOST_GRANT=lease["token"],
                       STORYHOOK_HOST_REQUEST=lease["id"], STORYHOOK_HOST_LEASE_FD=str(self.guard))
            self.child = subprocess.Popen(
                [sys.executable, "-B", str(Path(__file__).with_name("launcher.py")),
                 str(read_go), str(write_ready), str(self.guard), *command],
                start_new_session=True, pass_fds=(read_go, write_ready, self.guard), env=env)
            os.close(read_go); read_go = None
            os.close(write_ready); write_ready = None
            if not select.select([read_ready], [], [], self.timing["lease_ms"] / 1000)[0] or os.read(read_ready, 1) != b"R":
                raise Refusal("blocked launcher did not establish readiness")
            leader = native.identity(self.child.pid, self.boot)
            self.call("attach", execution=dict(id=self.execution_id, leader=leader,
                      session=self.child.pid, guard=guard_name))
            os.write(write_go, b"G")
        except BaseException:
            os.close(write_go); write_go = None
            if self.child:
                self.child.wait(timeout=self.timing["cleanup_ms"] / 1000)
            os.close(self.guard); self.guard = None
            raise
        finally:
            for fd in (read_go, write_go, read_ready, write_ready):
                if fd is not None:
                    os.close(fd)

    def call(self, operation, **arguments):
        """Use the same lease identity through cancellation and cleanup retries."""
        return self.client.call(operation, id=self.lease["id"], token=self.lease["token"], **arguments)

    def _exited(self):
        # Keep the leader waitable: its unreaped PID pins the process-group identity.
        return os.waitid(os.P_PID, self.child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None

    def _members(self):
        members = []
        for pid in native.session_members(self.child.pid):
            try:
                fact = native.process(pid, self.boot)
                if fact["live"]:
                    if os.getpgid(pid) != self.child.pid:
                        raise Refusal("managed descendant left its owned process group; cleanup is unknown")
                    members.append(pid)
            except ProcessLookupError:
                continue
        return members

    def wait(self):
        """Observe broker cancellation and drain only this pinned managed process group."""
        if self.finished:
            return self.child.returncode
        requested, sent_term, sent_kill = False, None, False
        prior = {}
        def cancel(_signal, _frame):
            nonlocal requested
            requested = True
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            prior[signum] = signal.signal(signum, cancel)
        try:
            while True:
                if requested:
                    self.call("cancel"); requested = False
                row = self.call("inspect")
                exited = self._exited()
                members = self._members()
                if exited and not members:
                    break
                if row["state"] in {"draining", "quarantined"} or exited:
                    now = time.monotonic_ns() // 1_000_000
                    if sent_term is None:
                        if members:
                            os.killpg(self.child.pid, signal.SIGTERM)
                        sent_term = now
                    elif now - sent_term >= self.timing["cleanup_ms"]:
                        if sent_kill:
                            raise Refusal("managed descendants did not settle after the cleanup allowance")
                        if members:
                            os.killpg(self.child.pid, signal.SIGKILL)
                        sent_kill = True
                        sent_term = now
                # This is the policy observation cadence, not a guessed workload delay.
                select.select([], [], [], self.timing["sample_ms"] / 1000)
            result = self.child.wait()
            os.close(self.guard); self.guard = None
            self.call("settle", execution_id=self.execution_id)
            if not self.call("finish"):
                raise Refusal("managed work ended but descendant settlement remains unproved")
            self.finished = True
            return result
        finally:
            for signum, handler in prior.items():
                signal.signal(signum, handler)

    def close(self):
        """Cancel and settle an owned child when a caller exits its supervision scope."""
        if self.child and not self.finished:
            self.call("cancel")
            self.wait()
        if self.guard is not None:
            os.close(self.guard); self.guard = None
