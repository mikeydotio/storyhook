"""Strict diagnostic admission; observations cannot construct repair authority."""

import os
import signal
import threading
import time

from .activation import load_policy
from .evidence import Publisher
from .namespace import ROOT
from .policy import Refusal
from .reservation import Reservation
from .supervisor import ManagedProcess


def monotonic():
    """The named POSIX clock shared with Rust, not the language default epoch."""
    return time.clock_gettime(time.CLOCK_MONOTONIC)


def _events(client, after, request):
    """Retain complete relevant pages; a bounded observation is never a prefix."""
    result = []
    while True:
        page = client.call("events", after=after)
        if not page:
            return result
        for event in page:
            if event["sequence"] <= after:
                raise Refusal("diagnostic resource event sequence did not advance")
            after = event["sequence"]
            if event.get("lease") in (None, request):
                result.append(event)
                if len(result) > 32768:
                    raise Refusal("diagnostic resource evidence exceeds its retention bound")


def _supported(policy, before, after, row, events, binding, ended, deadline):
    """Derive support from authoritative observations, independently of child exit."""
    identity = (before["authority"], before["policy"], before["boot"])
    if identity != (after["authority"], after["policy"], after["boot"]) or identity[1] != policy.digest:
        return False
    if ended >= deadline or before["pressure"] != "ready" or after["pressure"] != "ready":
        return False
    if row["binding"] != binding or row["state"] != "released" or row["resources"] != policy.value["workloads"]["causal-rust"]:
        return False
    peaks = row.get("peaks")
    if not peaks or set(peaks) != {"cpu", "memory"} or any(
            type(peaks[k]) is not int or not 0 <= peaks[k] <= row["resources"][k] for k in peaks):
        return False
    names = {event["event"] for event in events if event.get("lease") == row["id"]}
    if not {"grant", "attach", "usage", "cleanup", "release"}.issubset(names):
        return False
    for event in events:
        if identity != (event["authority"], event["policy"], event["boot"]):
            return False
        if event.get("lease") == row["id"] and event.get("binding") != binding:
            return False
        if event["event"] in {"cancel", "quarantine", "recovery"} or (
                event["event"] == "pressure" and event.get("reason") != "ready"):
            return False
    return True


def run(command, *, project, binding, journal, request_id, deadline,
        root=None, policy_loader=None, env=None):
    """Supervise one diagnostic pipeline and retain its actual resource evidence."""
    if deadline <= monotonic():
        raise Refusal("diagnostic active allowance is exhausted")
    reservation = Reservation("causal-rust", project=project, binding=binding,
                              request_id=request_id, root=ROOT if root is None else root,
                              policy_loader=load_policy if policy_loader is None else policy_loader)
    publisher = Publisher(reservation.client, binding, journal)
    # Record previous observations before selecting the new operation's cursor.
    publisher.publish()
    cursor = publisher.cursor
    before = reservation.client.call("status")
    interrupted = False
    def interrupt(_signum, _frame):
        nonlocal interrupted
        interrupted = True
    prior = {s: signal.signal(s, interrupt) for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
    timer = threading.Timer(max(0, deadline-monotonic()), lambda: os.kill(os.getpid(), signal.SIGTERM))
    managed = None
    timer.start()
    try:
        lease = reservation.acquire(interrupted=lambda: interrupted or monotonic() >= deadline)
        if lease is None:
            raise Refusal("diagnostic admission cancelled before launch")
        if interrupted or monotonic() >= deadline:
            raise Refusal("diagnostic allowance expired before launch")
        publisher.publish()
        managed = ManagedProcess(reservation.client, lease, command, publisher=publisher, env=env)
        result = managed.wait(force_cancel=interrupted or monotonic() >= deadline)
        managed.close()
        row = reservation.call("inspect", id=lease["id"], token=lease["token"])
        after = reservation.client.call("status")
        events = _events(reservation.client, cursor, lease["id"])
        publisher.publish()
        if row["state"] != "released" or any(not e["settled"] for e in row["executions"]):
            raise Refusal("diagnostic descendants have not settled")
        supported = not interrupted and managed.drain_reason is None and _supported(
            reservation.policy, before, after, row, events, binding, monotonic(), deadline)
        # Never publish bearer tokens; consumers retain identities, observations and references.
        retained = {key: row[key] for key in ("id", "state", "resources", "peaks", "binding", "reason")}
        return result, dict(version=1, binding=dict(binding), authority=after["authority"],
                            policy=after["policy"], boot=after["boot"], lease=retained, events=events,
                            supported=supported, cleanup_complete=True)
    finally:
        try:
            if managed is not None:
                managed.close()
            elif reservation.lease is not None:
                reservation.abandon()
        finally:
            timer.cancel()
            timer.join()
            for signum, handler in prior.items():
                signal.signal(signum, handler)
