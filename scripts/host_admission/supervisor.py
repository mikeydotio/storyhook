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


def inherited_descriptors():
    """The caller's open descriptors that a child would inherit across exec."""
    found = set()
    for name in os.listdir("/dev/fd"):
        fd = int(name)
        if fd > 2:
            try:
                if os.get_inheritable(fd):
                    found.add(fd)
            except OSError:
                continue  # the directory listing's own descriptor, now closed
    return found


class ManagedProcess:
    """A blocked-launch supervisor; SH-869 adapters own preserving its child contract."""

    def __init__(self, client, lease, command, *, publisher=None, env=None, grant_environment=True,
                 cwd=None, forward_signals=False):
        self.client, self.lease, self.child, self.guard = client, lease, None, None
        self.publisher = publisher
        self.forward_signals = forward_signals
        # The authority's reason for withdrawing this grant, once observed.
        self.drain_reason = None
        self.observation_failure = None
        self.finished = False
        self.result, self.failure = None, None
        self.execution_id = uuid.uuid4().hex
        self.boot = native.boot_identity()
        current = self.call("inspect")
        if current["state"] != "reserved":
            raise Refusal("managed launch requires a reserved grant")
        self.timing = client.call("status")["timing"]
        if not command or not all(isinstance(arg, str) and "\0" not in arg for arg in command):
            raise Refusal("managed command must be a nonempty argv")
        guard_name = f"lease-{lease['token']}.lock"
        self.guard_path = client.root / guard_name
        self.guard = open_private(self.guard_path, create=True)
        fcntl.flock(self.guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        read_go, write_go = os.pipe()
        read_ready, write_ready = os.pipe()
        read_exec, write_exec = os.pipe()
        try:
            env = dict(os.environ if env is None else env)
            if grant_environment:
                env.update(STORYHOOK_HOST_GRANT=lease["token"],
                           STORYHOOK_HOST_REQUEST=lease["id"],
                           STORYHOOK_HOST_LEASE_FD=str(self.guard))
            # Independent product custody may reuse the blocked launcher and
            # descendant settlement protocol, but must never manufacture a host
            # resource grant or replace an inherited grant (SH-835).
            # pass_fds forces close_fds; the caller's inheritable descriptors
            # (a Cargo jobserver, a lock holder's stdin) must still reach the
            # command, so they are passed explicitly beside the handshake.
            handshake = (read_go, write_ready, self.guard, write_exec)
            self.child = subprocess.Popen(
                [sys.executable, "-B", str(Path(__file__).with_name("launcher.py")),
                 str(read_go), str(write_ready), str(self.guard), str(write_exec), *command],
                start_new_session=True, pass_fds=tuple(sorted(set(handshake) | inherited_descriptors())),
                env=env, cwd=cwd)
            os.close(read_go); read_go = None
            os.close(write_ready); write_ready = None
            os.close(write_exec); write_exec = None
            if not select.select([read_ready], [], [], self.timing["lease_ms"] / 1000)[0] or os.read(read_ready, 1) != b"R":
                raise Refusal("blocked launcher did not establish readiness")
            leader = native.identity(self.child.pid, self.boot)
            self.call("attach", execution=dict(id=self.execution_id, leader=leader,
                      session=self.child.pid, guard=guard_name))
            os.write(write_go, b"G")
            if not select.select([read_exec], [], [], self.timing["lease_ms"] / 1000)[0]:
                raise Refusal("exec handshake exceeded the lease allowance")
            error = os.read(read_exec, 4096)
            if error:
                raise Refusal(f"managed exec failed: {error.decode(errors='replace')}")
        except BaseException as error:
            os.close(write_go); write_go = None
            if self.child:
                self.publisher = None
                try:
                    self.wait(force_cancel=True)
                except (Refusal, OSError) as cleanup:
                    raise Refusal(f"managed launch failed: {error}; cleanup: {cleanup}") from error
            elif self.guard is not None:
                os.close(self.guard); self.guard = None
            raise
        finally:
            for fd in (read_go, write_go, read_ready, write_ready, read_exec, write_exec):
                if fd is not None:
                    os.close(fd)

    def call(self, operation, **arguments):
        """Use the same lease identity through cancellation and cleanup retries."""
        return self.client.call(operation, id=self.lease["id"], token=self.lease["token"], **arguments)

    def _exited(self):
        # Keep the leader waitable: its unreaped PID pins the process-group identity.
        return os.waitid(os.P_PID, self.child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None

    def _members(self):
        # The session is the ownership boundary. Process groups inside it are
        # ordinary: captured children, `set -m` and fixtures create them.
        members = []
        for pid in native.session_members(self.child.pid):
            try:
                if native.session_member_is_live(pid, self.child.pid, self.boot):
                    members.append(pid)
            except ProcessLookupError:
                continue
            except OSError as error:
                # An unreadable participant is still possibly live. Retain it,
                # drain under the pinned session, and never certify this run.
                # _signal rechecks session membership before every delivery.
                self.observation_failure = f"cannot observe owned participant {pid}: {error}"
                members.append(pid)
        return members

    def _signal(self, members, signum):
        """Signal confirmed members of the pinned session; never a pid that left it."""
        for pid in members:
            try:
                if os.getsid(pid) == self.child.pid:
                    os.kill(pid, signum)
            except ProcessLookupError:
                continue

    def _guard_settled(self):
        # A PID census followed by liveness reads is not atomic: a member
        # can fork after enumeration and exit before its status is read.
        # Drop only our descriptor, never unlock the inherited description.
        # A fresh lock proves that every descendant has released that guard.
        if self.guard is not None:
            os.close(self.guard)
            self.guard = None
        probe = open_private(self.guard_path)
        try:
            try:
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return False
            return True
        finally:
            os.close(probe)

    def wait(self, *, force_cancel=False):
        """Observe broker cancellation and drain only this pinned managed process group."""
        if self.finished:
            if self.failure is not None:
                raise Refusal(self.failure)
            return self.result
        requested, sent_term, sent_kill = force_cancel, None, False
        first_signal = signal.SIGTERM
        draining, failure = force_cancel, None
        # TERM reaches each member once, so a handler that forks is not
        # re-triggered; after escalation every census member gets KILL,
        # including one forked after an earlier census.
        terminated = set()
        prior = {}
        def cancel(_signal, _frame):
            nonlocal requested, first_signal
            requested = True
            if self.forward_signals:
                first_signal = _signal
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            prior[signum] = signal.signal(signum, cancel)
        try:
            while True:
                if failure is None:
                    try:
                        if requested:
                            draining = True
                            self.call("cancel"); requested = False
                        row = self.call("inspect")
                        if row["state"] in {"draining", "quarantined"}:
                            draining = True
                            self.drain_reason = self.drain_reason or row.get("reason")
                        if self.publisher:
                            self.publisher.publish()
                    except (Refusal, OSError) as error:
                        failure, draining = str(error), True
                exited = self._exited()
                members = self._members()
                if self.observation_failure is not None:
                    failure = failure or self.observation_failure
                    draining = True
                    if not requested:
                        requested = True
                if exited and not members and self._guard_settled():
                    break
                if draining or exited:
                    now = time.monotonic_ns() // 1_000_000
                    if sent_term is None:
                        sent_term = now
                    elif now - sent_term >= self.timing["cleanup_ms"]:
                        if sent_kill:
                            raise Refusal("managed descendants did not settle after the cleanup allowance")
                        sent_kill = True
                        sent_term = now
                    if sent_kill:
                        self._signal(members, signal.SIGKILL)
                    else:
                        self._signal([pid for pid in members if pid not in terminated], first_signal)
                        terminated.update(members)
                # This is the policy observation cadence, not a guessed workload delay.
                select.select([], [], [], self.timing["sample_ms"] / 1000)
            result = self.child.wait()
            self.result = 125 if draining and result == 0 else result
            if self.guard is not None:
                os.close(self.guard); self.guard = None
            self.finished = True
            if failure:
                raise Refusal(f"owned processes drained; admission control or evidence failed: {failure}")
            row = self.call("inspect")
            if any(e["id"] == self.execution_id for e in row["executions"]):
                self.call("settle", execution_id=self.execution_id)
            if not self.call("finish"):
                raise Refusal("managed work ended but descendant settlement remains unproved")
            if self.publisher:
                self.publisher.publish()
            return self.result
        except (Refusal, OSError) as error:
            if self.finished:
                self.failure = str(error)
            raise
        finally:
            for signum, handler in prior.items():
                signal.signal(signum, handler)

    def close(self):
        """Cancel and settle an owned child when a caller exits its supervision scope."""
        if self.child and not self.finished:
            # Journal failure must not prevent cleanup of the owned process group.
            self.publisher = None
            self.wait(force_cancel=True)
        if self.guard is not None:
            os.close(self.guard); self.guard = None
