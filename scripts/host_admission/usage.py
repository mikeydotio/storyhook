"""Native sampled resource peaks for registered execution sessions."""

import ctypes
import os
from pathlib import Path
import sys
import time

from . import native
from .policy import Refusal, integer


class Rusage(ctypes.Structure):
    """Darwin rusage_info_v0, from the public sys/resource.h ABI."""

    _fields_ = [("uuid", ctypes.c_ubyte * 16)] + [
        (key, ctypes.c_uint64) for key in ("user", "system", "idle_wakeups", "interrupt_wakeups",
        "pageins", "wired", "resident", "footprint", "start", "exit")]


def counters(pid):
    """Read cumulative CPU nanoseconds and resident bytes from the kernel."""
    if sys.platform == "darwin":
        info = Rusage()
        lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        if lib.proc_pid_rusage(pid, 0, ctypes.byref(info)):
            raise OSError(ctypes.get_errno(), f"cannot observe resource counters for PID {pid}")
        return dict(cpu_ns=info.user + info.system, memory=info.resident)
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    except FileNotFoundError:
        raise ProcessLookupError(f"PID {pid} exited") from None
    if len(fields) < 22:
        raise Refusal("incomplete native process resource counters")
    return dict(cpu_ns=(int(fields[11]) + int(fields[12])) * 1_000_000_000 // os.sysconf("SC_CLK_TCK"),
                memory=int(fields[21]) * os.sysconf("SC_PAGE_SIZE"))


def session_snapshot(session, boot):
    """Corroborate every counter with the same native process incarnation."""
    rows = {}
    for pid in native.session_members(session):
        try:
            before = native.identity(pid, boot)
            value = counters(pid)
            after = native.identity(pid, boot)
            if before != after or os.getsid(pid) != session:
                raise Refusal("resource observation crossed a process incarnation or session change")
            rows[f"{pid}:{before['start']}"] = value
        except ProcessLookupError:
            continue
    return rows

class Monitor:
    """Keep a per-envelope CPU baseline without treating missing observations as idle."""

    def __init__(self, boot, read=None, clock=None):
        self.read = read or (lambda session: session_snapshot(session, boot))
        self.clock = clock or time.monotonic_ns
        self.previous = {}

    def sample(self, identity, sessions):
        """Observe one lease and all sessions partitioned from its envelope."""
        rows = {}
        for session in set(sessions):
            for key, value in self.read(session).items():
                if key in rows:
                    raise Refusal("resource census assigned one process to multiple sessions")
                integer(value["cpu_ns"], "CPU counter", 0)
                integer(value["memory"], "resident memory", 0)
                rows[key] = value.copy()
        now = self.clock()
        previous = self.previous.get(identity)
        cpu = None
        if previous:
            then, old = previous
            if now <= then:
                raise Refusal("resource interval clock did not advance")
            if rows.keys() == old.keys():
                if any(rows[k]["cpu_ns"] < old[k]["cpu_ns"] for k in rows):
                    raise Refusal("native CPU counter moved backward")
                delta = sum(rows[k]["cpu_ns"] - old[k]["cpu_ns"] for k in rows)
                cpu = (delta * 1000 + now - then - 1) // (now - then)
        self.previous[identity] = now, rows
        return dict(cpu=cpu, memory=sum(row["memory"] for row in rows.values()))
