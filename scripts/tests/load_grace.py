"""Contention grace for verifier fixture budgets and waits (SH-767).

A port of the browser suite's policy (e2e/load-grace.ts, SH-347), whose user
determination reads: relax the timeouts when the machine is under load, up to
a maximum of 15 minutes, rather than ending the test. At or below one runnable
thread per core every value here is exactly the idle value, so a real defect
surfaces as fast as it did before grace existed.

The multiplier assumes fair processor sharing: a thread's wall clock stretches
by the number of runnable threads per core. A fixture run at a lowered quality
of service (SH-785) can stretch further than that.
"""

import math
import os
import sys


def cores():
    """Return the logical cores this process may run on, never zero.

    process_cpu_count (Python 3.13+) honours an affinity or container limit;
    older interpreters, which this harness still supports, use cpu_count.
    """
    counter = getattr(os, "process_cpu_count", None)
    return (counter() if counter else None) or os.cpu_count() or 1


def contention():
    """Return runnable threads per core over the last minute, or None if unknown.

    A one-minute average reacts late to a burst and lingers after one; a late
    reading costs one wait its extension, a lingering one only extra patience.
    """
    try:
        return os.getloadavg()[0] / cores()
    except OSError:
        return None


def multiplier(ratio, maximum):
    """Return the grace for one contention reading, capped at maximum.

    Exactly 1 at or below one thread per core, or with no reading at all.
    """
    if ratio is None:
        return 1.0
    return min(maximum, max(1.0, ratio))


def graced_spelling(milliseconds, grace):
    """Scale a decimal millisecond spelling by grace, keeping its leading zeros.

    The spelling is returned unchanged at grace 1. The product is rounded to
    a thousandth before rounding up, so binary noise cannot add a millisecond.
    """
    if grace == 1:
        return milliseconds
    zeros = len(milliseconds) - len(milliseconds.lstrip("0"))
    return "0" * zeros + str(math.ceil(round(int(milliseconds) * grace, 3)))


def describe(ratio, grace):
    """Name one contention reading and the grace chosen from it."""
    reading = "unavailable" if ratio is None else f"{ratio:.2f}"
    return f"load-grace: contention={reading} cores={cores()} multiplier={grace:.2f}"


class Patience:
    """A wait allowance that extends, and never shrinks, while contention rises.

    Playwright cannot resume a fired timeout, so SH-347 extends a deadline
    before it fires; this does the same at each expiry. The caller supplies
    every clock reading, so a wait reads the clock exactly as often as before.
    """

    def __init__(self, allowance, grace, maximum, started):
        """Start an allowance already graced by grace at clock reading started."""
        self.idle = allowance / grace
        self.grace = grace
        self.maximum = maximum
        self.started = started
        self.allowance = allowance

    def expired(self, now):
        """Report whether the allowance has elapsed at now, after any extension."""
        if now - self.started < self.allowance:
            return False
        ratio = contention()
        grace = multiplier(ratio, self.maximum)
        if grace > self.grace:
            self.grace = grace
            self.allowance = self.idle * grace
            print(f"load-grace: extended a wait to {self.allowance:.3f}s; {describe(ratio, grace)}",
                  file=sys.stderr)
        return now - self.started >= self.allowance

    def remaining(self, now):
        """Return the seconds left in the current allowance at now."""
        return max(0, self.started + self.allowance - now)
