"""Read native process incarnation and executable evidence on macOS and Linux."""

import ctypes
import errno
import os
from pathlib import Path
import sys


class BsdInfo(ctypes.Structure):
    """The public PROC_PIDTBSDINFO layout from Apple's sys/proc_info.h."""

    _fields_ = [(name, ctypes.c_uint32) for name in (
        "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid",
        "rgid", "svuid", "svgid", "reserved")]
    _fields_ += [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
    _fields_ += [(name, ctypes.c_uint32) for name in (
        "nfiles", "pgid", "pjobc", "tdev", "tpgid")]
    _fields_ += [("nice", ctypes.c_int32), ("seconds", ctypes.c_uint64),
                 ("microseconds", ctypes.c_uint64)]


def process_identity(pid):
    """Return kernel-backed identity; a dead process raises ProcessLookupError.

    Exit, a zombie that its parent has not reaped, and a missing /proc entry
    all mean the incarnation is gone, so callers can tell exit apart from a
    probe that failed (any other OSError, such as a denied read).
    """
    if type(pid) is not int or pid <= 1:
        raise ValueError("invalid process ID")
    if sys.platform == "darwin":
        lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                    ctypes.c_void_p, ctypes.c_int]
        lib.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        info = BsdInfo()
        size = ctypes.sizeof(info)
        if lib.proc_pidinfo(pid, 3, 0, ctypes.byref(info), size) != size:
            raise OSError(ctypes.get_errno(), f"cannot read process {pid} incarnation")
        if info.status == 5:  # SZOMB: exited, awaiting its parent's wait().
            raise ProcessLookupError(errno.ESRCH, f"process {pid} is not live")
        if info.pid != pid:
            raise OSError(f"process {pid} returned the identity of process {info.pid}")
        path = ctypes.create_string_buffer(4096)
        if lib.proc_pidpath(pid, path, len(path)) <= 0:
            raise OSError(ctypes.get_errno(), f"cannot read process {pid} executable")
        start = f"macos:{info.seconds}:{info.microseconds}"
        executable = os.fsdecode(path.value)
    elif sys.platform.startswith("linux"):
        # comm may contain spaces or ')'; stat fields start after its last ')'.
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        except FileNotFoundError:
            raise ProcessLookupError(errno.ESRCH, f"process {pid} is not live") from None
        if fields and fields[0] in ("Z", "X"):
            raise ProcessLookupError(errno.ESRCH, f"process {pid} is not live")
        if len(fields) < 20:
            raise OSError(f"process {pid} has a malformed /proc stat record")
        start = f"linux:{fields[19]}"
        try:
            executable = os.readlink(f"/proc/{pid}/exe")
        except FileNotFoundError:
            raise ProcessLookupError(errno.ESRCH, f"process {pid} exited during identity capture") from None
    else:
        raise OSError(f"process identity is unsupported on {sys.platform}")
    if not os.path.isabs(executable):
        raise OSError(f"process {pid} has no absolute executable path")
    return {"pid": pid, "start": start, "executable": os.path.realpath(executable)}
