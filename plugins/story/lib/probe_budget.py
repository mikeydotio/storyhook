"""One time budget for each plugin helper operation, shared by its probes (SH-766).

A helper that freezes processes must resume them before its caller gives up,
and a probe must not fail while the caller still has time. So each helper
invocation runs as one operation with one deadline, and each external probe
(ps, tmux, git) gets whatever remains of it.

The budget is two thirds of the tightest caller bound. The daemon runs
`story.sh notify` for 45 s (NOTIFY_TIMEOUT, then SIGTERM) and
`dropped-cleanup-pane.py` for 45 s (CLEANUP_HELPER_TIMEOUT, SIGKILL with no
SIGTERM). The last third covers interpreter start and exit under load. Unit
tests beside those constants pin the relation.

Machine load is deliberately not a multiplier here. Spawn latency under load
is not proportional to load per core (SH-643 measured 250 times the latency
for 5 times the contention), and a multiplied bound would cross the callers'
fixed bounds. Each timeout reports the 1-minute load average instead, so the
evidence shows the contention.
"""

import contextlib
import contextvars
import os
import subprocess
import time

# Seconds for one helper operation: two thirds of the 45 s caller bounds above.
BUDGET_SECONDS = 30

# (started, budget) of the operation this context runs inside, if any.
_operation = contextvars.ContextVar("probe_budget_operation", default=None)


class ProbeTimeout(subprocess.TimeoutExpired):
    """A probe the operation budget ended, with the evidence to diagnose it.

    It is a TimeoutExpired, so every existing handler still catches it.
    """

    def __init__(self, cmd, allowance, budget, elapsed):
        """Record the probe, its allowance, the budget, the time spent and the load."""
        super().__init__(cmd, allowance)
        self.budget = budget
        self.elapsed = elapsed
        try:
            self.load = f"{os.getloadavg()[0]:.2f}"
        except OSError:
            self.load = "unavailable"

    def __str__(self):
        """Name the probe and every fact that explains why it had no more time."""
        command = " ".join(str(part) for part in self.cmd)
        return (f"probe '{command}' did not finish in its {self.timeout:.1f}s allowance "
                f"({self.elapsed:.1f}s of a {self.budget:g}s operation budget spent; "
                f"1-minute load average {self.load} on {os.cpu_count()} cores)")


@contextlib.contextmanager
def operation(budget=BUDGET_SECONDS):
    """Give every probe inside this block one shared deadline of budget seconds.

    An operation inside another keeps the outer deadline: a callee cannot
    extend the time its caller granted.
    """
    if _operation.get() is not None:
        yield
        return
    token = _operation.set((time.monotonic(), budget))
    try:
        yield
    finally:
        _operation.reset(token)


def remaining():
    """Return the seconds left for the next probe.

    Inside an operation this is what remains of its deadline, never below
    zero. Outside one, a probe gets one whole budget.
    """
    current = _operation.get()
    if current is None:
        return float(BUDGET_SECONDS)
    started, budget = current
    return max(0.0, started + budget - time.monotonic())


def _spent(probe_started):
    """Return (budget, elapsed seconds) of the current operation, else of this probe."""
    started, budget = _operation.get() or (probe_started, BUDGET_SECONDS)
    return budget, time.monotonic() - started


def run(argv, **kwargs):
    """Run one external probe within the time its operation has left.

    A probe never starts once the budget is spent. Other keyword arguments
    go to subprocess.run unchanged; the timeout is always this budget's.
    """
    probe_started = time.monotonic()
    allowance = remaining()
    if allowance <= 0:
        raise ProbeTimeout(argv, 0, *_spent(probe_started))
    try:
        return subprocess.run(argv, timeout=allowance, **kwargs)
    except subprocess.TimeoutExpired as error:
        raise ProbeTimeout(argv, allowance, *_spent(probe_started)) from error
