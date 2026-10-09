"""Representative host observations: contention is evidence, not an idle gate."""

import ctypes
import math
import os
import sys
import time

from verifier_state import Refusal

PROTOCOL = {'version': 2, 'host_load': 'representative-variable',
            'load_average_rejection': False, 'natural_contention_rejection': False,
            'memory_pressure': 'known-normal', 'performance_inference': 'review-required'}


def cpu_ticks():
    """Read cumulative Darwin ticks without sleeping or launching a process."""
    if sys.platform != 'darwin':
        raise Refusal('representative host collection currently requires macOS')
    libc = ctypes.CDLL(None, use_errno=True)
    libc.mach_host_self.restype = ctypes.c_uint
    libc.host_statistics.argtypes = [ctypes.c_uint, ctypes.c_int,
                                    ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_uint)]
    host = libc.mach_host_self()
    values, count = (ctypes.c_int * 4)(), ctypes.c_uint(4)
    try:
        if libc.host_statistics(host, 3, values, ctypes.byref(count)) or count.value != 4:
            raise Refusal('native CPU tick observation failed')
        return [value & 0xffffffff for value in values]  # user, system, idle, nice
    finally:
        task = ctypes.c_uint.in_dll(libc, 'mach_task_self_').value
        if libc.mach_port_deallocate(task, host):
            raise Refusal('native CPU observation port release failed')


def snapshot():
    """Cheap native observations for the small local Git experiment."""
    from gate_measurement_storage import pressure_level
    from host_admission.native import sysctl
    class Swap(ctypes.Structure):
        _fields_ = [('total', ctypes.c_uint64), ('available', ctypes.c_uint64),
                    ('used', ctypes.c_uint64), ('page_size', ctypes.c_uint32),
                    ('encrypted', ctypes.c_int)]
    raw = sysctl('vm.swapusage')
    if len(raw) != ctypes.sizeof(Swap):
        raise Refusal('native swap observation has an unknown shape')
    swap = Swap.from_buffer_copy(raw)
    if swap.used > swap.total or swap.available > swap.total or not swap.page_size:
        raise Refusal('native swap observation is invalid')
    result = {'monotonic': time.monotonic(), 'load': list(os.getloadavg()),
              'cores': os.cpu_count(), 'native_memory_pressure': pressure_level(),
              'cpu_ticks': cpu_ticks(), 'swap_used_bytes': swap.used}
    validate_exposure(result)
    return result


def validate_exposure(row):
    """Require known sensors; any finite nonnegative CPU load remains admissible."""
    loads = row.get('load')
    if (not isinstance(loads, list) or len(loads) != 3
            or any(type(x) not in (int, float) or not math.isfinite(x) or x < 0 for x in loads)
            or type(row.get('cores')) is not int or row['cores'] <= 0
            or type(row.get('native_memory_pressure')) is not int
            or row['native_memory_pressure'] != 1):
        raise Refusal('unknown or unsafe representative host observation')
    ticks = row.get('cpu_ticks')
    if (not isinstance(ticks, list) or len(ticks) != 4
            or any(type(x) is not int or not 0 <= x <= 0xffffffff for x in ticks)):
        raise Refusal('unknown native CPU tick observation')
    if type(row.get('monotonic')) not in (int, float) or not math.isfinite(row['monotonic']):
        raise Refusal('unknown host observation time')
    return row


def cpu_interval(before, after):
    """Average over observed tick boundaries, never infer saturation from load."""
    validate_exposure(before); validate_exposure(after)
    seconds = after['monotonic'] - before['monotonic']
    if seconds <= 0 or before['cores'] != after['cores']:
        raise Refusal('CPU interval changed cores or observation clock')
    ticks = [(b - a) & 0xffffffff for a, b in zip(before['cpu_ticks'], after['cpu_ticks'])]
    total = sum(ticks)
    return {'seconds': seconds, 'idle_percent': 100 * ticks[2] / total if total else None,
            'user_percent': 100 * (ticks[0] + ticks[3]) / total if total else None,
            'system_percent': 100 * ticks[1] / total if total else None,
            'tick_deltas': ticks}


def annotate_processes(observed, owner_pid):
    """Classify natural external work without rejecting it or signalling its owners."""
    from gate_measurement_campaign import competing_work, owned_resources
    validate_exposure(observed)
    observed['competing_pids'] = competing_work(observed['processes'], owner_pid)
    observed['owned_resources'] = owned_resources(observed['resource_processes'], owner_pid)
    observed['host_load_protocol'] = PROTOCOL
    return observed
