"""Measurement containment policy; these ceilings do not relax acceptance."""

import datetime
import math
import time

from verifier_state import Refusal

LIMITS = {
    "campaign_seconds": 80100,  # 22.25 hours, including preparation/waits
    "gate_seconds": 3600,
    "preparation_seconds": 2400,
    "probe_preparation_seconds": 600,
    "quiet_wait_seconds": 300,
    "quiet_seconds": 60,
    "sample_seconds": 5,
    "lock_wait_seconds": 60,
    "probe_seconds": 30,
    "capture_seconds": 30,
    "cleanup_seconds": 35,  # existing 30 s supervisor grace plus observation
    "full_gate_slots": 21,
    "production_target_seconds": 900,
}


class Deadline:
    """An injected monotonic clock makes exact boundaries testable without sleep."""

    def __init__(self, seconds, *, clock=time.monotonic, end=None):
        if type(seconds) not in (int, float) or not math.isfinite(seconds) or seconds <= 0:
            raise Refusal("measurement deadline must be positive and finite")
        self.clock = clock
        self.started = clock()
        self.end = min(self.started + seconds, end) if end is not None else self.started + seconds

    def remaining(self):
        now = self.clock()
        if now < self.started:
            raise Refusal("measurement monotonic clock moved backwards")
        return max(0, self.end - now)

    def require(self, label, allowance=0):
        left = self.remaining()
        if left <= 0 or left < allowance:
            raise Refusal(f"{label} exceeds remaining measurement allowance ({left:.3f}s)")
        return left

    def child(self, seconds):
        return Deadline(seconds, clock=self.clock, end=self.end)


def require_same_day(day, allowance, *, now=None):
    """No gate starts unless its full ceiling fits the original local day."""
    now = datetime.datetime.now().astimezone() if now is None else now
    if now.date().isoformat() != day:
        raise Refusal("measurement local day changed")
    after = datetime.datetime.fromtimestamp(now.timestamp() + allowance, now.tzinfo)
    if after.date() != now.date():
        raise Refusal("gate allowance would cross the local day boundary")


def validate_policy(identity):
    """A historical or changed manifest cannot silently run the bounded protocol."""
    if identity.get("limits") != LIMITS:
        raise Refusal("measurement manifest lacks the exact bounded policy")


def start_slot(events, kind, index):
    """Every attempted warmup/gate consumes one immutable slot, including failures."""
    starts = [row for row in events if row.get("kind") == "gate-start"]
    if len(starts) >= LIMITS["full_gate_slots"]:
        raise Refusal("measurement full-gate cap exhausted")
    if any(row.get("sample_kind") == kind and row.get("index") == index for row in starts):
        raise Refusal("measurement gate slot already consumed; retries cannot replace evidence")
    if any(not any(end.get("kind") == "gate-settled" and end.get("slot") == row.get("slot")
                   for end in events) for row in starts):
        raise Refusal("earlier measurement gate has no settlement evidence")
    return {"kind": "gate-start", "slot": len(starts), "sample_kind": kind, "index": index}
