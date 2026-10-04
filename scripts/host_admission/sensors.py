"""Native pressure sensors; missing kernel evidence is never fabricated."""

import ctypes
from decimal import Decimal, InvalidOperation
from pathlib import Path
import os
import sys
import time
import subprocess

from . import native
from .policy import Refusal


class Sensor:
    """Measure CPU deltas and preserve contemporaneous memory and runnable observations."""

    def __init__(self, read=None, clock=None, *, freshness_ms=None):
        if read is None and sys.platform == "darwin" and not freshness_ms:
            raise Refusal("native sensor requires a calibrated freshness budget")
        self.read = read or ((lambda: darwin_snapshot(freshness_ms / 1000)) if sys.platform == "darwin" else
                             lambda: linux_snapshot(lambda name: (Path("/proc") / name).read_text()))
        self.clock = clock or (lambda: time.monotonic_ns() // 1_000_000)
        self.previous = None

    def __call__(self):
        """Return unknown until two valid cumulative CPU readings establish an interval."""
        row = self.read()
        ticks = row.pop("cpu_ticks")
        previous, self.previous = self.previous, ticks
        if previous is None:
            return None
        busy, total = (ticks[i] - previous[i] for i in (0, 1))
        if total <= 0 or not 0 <= busy <= total:
            self.previous = None
            raise Refusal("native CPU counters did not advance consistently")
        cpu = max((busy * 1000 + total - 1) // total, row.pop("cpu_stall", 0))
        return dict(row, cpu=cpu, at=self.clock())


def _psi(text):
    """Convert PSI's native percentage to integer permille, rounding upward."""
    try:
        line = next(line for line in text.splitlines() if line.startswith("some "))
        fields = dict(field.split("=", 1) for field in line.split()[1:])
        value = Decimal(fields["avg10"])
        if not value.is_finite() or not 0 <= value <= 100:
            raise ValueError("out-of-range PSI")
        return int((value * 10).to_integral_value(rounding="ROUND_CEILING"))
    except (StopIteration, KeyError, ValueError, InvalidOperation) as error:
        raise Refusal(f"invalid pressure stall data: {error}") from error


def linux_snapshot(read):
    """Read native procfs formats through an injectable read-only input."""
    try:
        memory = {line.split(":")[0]: line.split(":")[1].split()
                  for line in read("meminfo").splitlines()}
        available, unit = memory["MemAvailable"]
        if unit != "kB":
            raise ValueError("unexpected memory unit")
        stat = read("stat").splitlines()
        ticks = [int(v) for v in next(s for s in stat if s.startswith("cpu ")).split()[1:9]]
        if len(ticks) != 8 or min(ticks) < 0:
            raise ValueError("incomplete CPU counters")
        runnable = int(next(s for s in stat if s.startswith("procs_running ")).split()[1])
        return dict(cpu_ticks=(sum(ticks) - ticks[3] - ticks[4], sum(ticks)),
                    available=int(available) * 1024, runnable=runnable,
                    memory=_psi(read("pressure/memory")), cpu_stall=_psi(read("pressure/cpu")))
    except (KeyError, ValueError, StopIteration) as error:
        raise Refusal(f"invalid native Linux resource data: {error}") from error


_HOST_PORT = None


def _host_statistics(flavor, count):
    """Read the stable public Mach host_statistics ABI; retain one process-local send right."""
    global _HOST_PORT
    lib = ctypes.CDLL(None)
    if _HOST_PORT is None:
        _HOST_PORT = lib.mach_host_self()
    data = (ctypes.c_uint32 * count)()
    size = ctypes.c_uint32(count)
    result = lib.host_statistics(_HOST_PORT, flavor, data, ctypes.byref(size))
    if result or size.value != count:
        raise Refusal(f"Mach host_statistics({flavor}) failed: {result}, count {size.value}")
    return list(data)


def runnable_states(text):
    """Bound runnable processes above by counting each protected/unknown state as runnable."""
    states = text.splitlines()
    if not states or any(not s.strip() or s.strip()[0] not in "RSDITZUX?" or
                         any(c not in "<NLEs+lVWX" for c in s.strip()[1:]) for s in states):
        raise Refusal("invalid native runnable-process census")
    return sum(s.strip()[0] in "R?" for s in states)


def darwin_snapshot(timeout):
    """Read CPU ticks, conservative reclaimable pages, kernel pressure and runnable processes."""
    ticks = _host_statistics(3, 4)  # HOST_CPU_LOAD_INFO; user, system, idle, nice.
    vm = _host_statistics(2, 15)  # HOST_VM_INFO rev2, beginning with free/active/inactive/wired.
    level = int.from_bytes(native.sysctl("kern.memorystatus_vm_pressure_level"), sys.byteorder)
    # The sysctl exports dispatch flags, not the kernel's internal enum.
    if level not in (1, 2, 4):
        raise Refusal(f"unknown Darwin pressure flag {level}")
    try:
        result = subprocess.run(["/bin/ps", "-axo", "stat="], capture_output=True,
                                text=True, check=True, timeout=timeout)
        runnable = runnable_states(result.stdout)
    except (subprocess.SubprocessError, UnicodeError) as error:
        raise Refusal(f"native runnable-process census failed: {error}") from error
    return dict(cpu_ticks=(sum(ticks) - ticks[2], sum(ticks)),
                available=(vm[0] + vm[2]) * os.sysconf("SC_PAGE_SIZE"),
                memory={1: 0, 2: 500, 4: 1000}[level], runnable=runnable)
