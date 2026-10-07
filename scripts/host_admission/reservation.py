"""A root grant for one managed entry, held by the process that supervises it (SH-869).

`host-admit.py` holds a root through `ManagedProcess`; `verifier-owner.py`
holds the verifier gate's root around its own setsid session. Both acquire,
watch and release the grant here, so one lifecycle and one cause taxonomy
serve every root.
"""

import fcntl
import os
import select
import uuid

from .activation import load_policy
from .client import Client
from .entries import ENTRIES
from .namespace import ROOT, open_private
from .native import host_identity, identity, boot_identity
from .policy import Refusal

# Broker drain reasons (authority.py, scheduler.drain) and refusal classes,
# mapped to a typed cause and whether a retry can succeed without a change.
CAUSES = {
    "severe pressure": ("pressure", True),
    "lease deadline": ("lease-deadline", False),
    "resource envelope exceeded": ("envelope-exceeded", False),
}


class Admission(Exception):
    """Admission refused or a grant withdrawn; never a verdict on the work itself."""

    def __init__(self, cause, retryable, reason):
        super().__init__(f"host admission {cause}: {reason}")
        self.cause, self.retryable, self.reason = cause, retryable, reason


def drain_cause(reason):
    """The typed cause of a broker-initiated drain; quarantine reasons are sensor faults."""
    if reason in CAUSES:
        return CAUSES[reason]
    return ("sensor", True)


def amount(policy, name):
    """One named measured workload; an incomplete calibration is a permanent refusal."""
    workloads = policy.value["workloads"]
    if name not in workloads:
        raise Admission("workload-missing", False,
                        f"the host policy has no measured workload named {name!r}")
    return dict(workloads[name])


def plan(entry, requested, policy, *, work=None):
    """(units, reserved resources, per-unit share) for a root of `entry`."""
    work = work or entry.work
    unit = amount(policy, entry.unit)
    overhead = amount(policy, entry.overhead) if entry.overhead else dict(cpu=0, memory=0)
    limit = policy.cap if work == "repair" else policy.normal
    room = {k: limit[k] - overhead[k] for k in unit}
    fit = min(room[k] // unit[k] for k in unit) if min(room.values()) > 0 else 0
    if fit < 1:
        raise Admission("capacity", False, f"one {entry.id} unit exceeds the host's {work} capacity")
    units = max(1, min(requested, fit)) if entry.pool else 1
    reserved = {k: overhead[k] + units * unit[k] for k in unit}
    return units, reserved, {k: unit[k] if entry.pool else reserved[k] - overhead[k] for k in unit}


class Reservation:
    """One root request: enqueue, wait, watch for drains, release after proof."""

    def __init__(self, entry_id, *, project, requested=1, work=None, binding=None,
                 root=ROOT, policy_loader=load_policy, request_id=None):
        self.entry = ENTRIES[entry_id]
        if work not in (None, self.entry.work, "repair"):
            raise Refusal(f"entry {entry_id} cannot be admitted as {work}")
        self.root = root
        try:
            self.policy = policy_loader(root, host_identity())
        except Refusal as error:
            raise Admission("invalid-policy", False, str(error)) from error
        self.work = work or self.entry.work
        self.units, self.resources, self.share = plan(self.entry, requested, self.policy, work=self.work)
        self.client = Client(root, timeout_ms=self.policy.value["stale_ms"])
        self.id = request_id or f"{entry_id}:{uuid.uuid4().hex}"
        self.project, self.binding = project, binding
        self.lease, self.guard, self.execution = None, None, None

    def call(self, operation, **arguments):
        """Lease operations; a broker that cannot be reached is a retryable cause."""
        try:
            return self.client.call(operation, **arguments)
        except FileNotFoundError as error:
            raise Admission("broker-unavailable", True, f"no host admission broker: {error}") from error
        except ConnectionError as error:
            raise Admission("broker-unavailable", True, str(error)) from error
        except Refusal as error:
            # A lost reply or deadline is transport, not a decision about the request.
            if str(error).startswith("host broker"):
                raise Admission("broker-unavailable", True, str(error)) from error
            raise

    def acquire(self, waiting=None, interrupted=lambda: False):
        """Enqueue and wait at the policy's sample cadence; `waiting(row)` reports each turn."""
        request = dict(id=self.id, project=self.project, work=self.work, resources=self.resources)
        if self.binding is not None:
            request["binding"] = self.binding
        try:
            self.lease = self.call("enqueue", request=request)
            while self.lease["state"] == "queued":
                if interrupted():
                    self.abandon()
                    return None
                if waiting:
                    waiting(self.lease)
                self.lease = self.call("wait", id=self.lease["id"], token=self.lease["token"])
        except Refusal as error:
            if self.lease is not None:
                self.abandon()
            raise Admission("refused", False, str(error)) from error
        if self.lease["state"] != "reserved":
            reason = self.lease.get("reason") or self.lease["state"]
            self.abandon()
            raise Admission(*drain_cause(reason), reason)
        return self.lease

    def abandon(self):
        """Cancel a request that never launched; only proven cleanup frees it."""
        if self.lease is None:
            return
        self.call("cancel", id=self.lease["id"], token=self.lease["token"])
        if not self.call("finish", id=self.lease["id"], token=self.lease["token"]):
            raise Refusal("reservation retained until independent cleanup is proved")

    def child_env(self, units_env=True):
        """The variables that make every descendant run inside this grant."""
        env = dict(STORYHOOK_HOST_GRANT=self.lease["token"], STORYHOOK_HOST_REQUEST=self.lease["id"],
                   STORYHOOK_HOST_SHARE=f"cpu={self.share['cpu']},memory={self.share['memory']}")
        if self.guard is not None:
            env["STORYHOOK_HOST_LEASE_FD"] = str(self.guard)
        if units_env and self.entry.pool:
            env["STORYHOOK_HOST_UNITS"] = str(self.units)
        return env

    def open_guard(self):
        """The lease lifetime guard, held here and inherited by the supervised session."""
        name = f"lease-{self.lease['token']}.lock"
        self.guard = open_private(self.client.root / name, create=True)
        fcntl.flock(self.guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return self.guard

    def attach(self, leader):
        """Commit the supervised session before its command may run (blocked launch)."""
        self.execution = uuid.uuid4().hex
        self.call("attach", id=self.lease["id"], token=self.lease["token"],
                  execution=dict(id=self.execution, leader=identity(leader, boot_identity()),
                                 session=leader, guard=f"lease-{self.lease['token']}.lock"))

    def drained(self):
        """(cause, retryable, reason) once the authority withdraws this grant, else None."""
        row = self.call("inspect", id=self.lease["id"], token=self.lease["token"])
        if row["state"] in {"draining", "quarantined"} and row.get("reason") != "client cancellation":
            cause, retryable = drain_cause(row.get("reason"))
            return cause, retryable, row.get("reason")
        return None

    def cancel(self):
        """Ask the authority to drain this grant; capacity stays held until release."""
        self.call("cancel", id=self.lease["id"], token=self.lease["token"])

    def release(self):
        """Settle after the supervisor proved its session empty and closed its guard."""
        if self.guard is not None:
            os.close(self.guard)
            self.guard = None
        if self.execution is not None:
            self.call("settle", id=self.lease["id"], token=self.lease["token"],
                      execution_id=self.execution)
        if not self.call("finish", id=self.lease["id"], token=self.lease["token"]):
            raise Refusal("managed work ended but descendant settlement remains unproved")

    def pause(self):
        """Wait one policy sample interval, never a guessed delay."""
        select.select([], [], [], self.policy.value["sample_ms"] / 1000)
