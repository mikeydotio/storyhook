"""Kernel host/process identities and session observations for macOS and Linux."""

import ctypes
import errno
import os
from pathlib import Path
import socket
import struct
import sys
import uuid

from .policy import Refusal, integer


class BsdInfo(ctypes.Structure):
    """PROC_PIDTBSDINFO ABI from the public Darwin sys/proc_info.h header."""

    _fields_ = [(name, ctypes.c_uint32) for name in
                ("flags", "status", "xstatus", "pid", "ppid", "uid", "gid",
                 "ruid", "rgid", "svuid", "svgid", "reserved")]
    _fields_ += [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
    _fields_ += [(name, ctypes.c_uint32) for name in ("nfiles", "pgid", "pjobc", "tdev", "tpgid")]
    _fields_ += [("nice", ctypes.c_int32), ("seconds", ctypes.c_uint64), ("microseconds", ctypes.c_uint64)]


def sysctl(name):
    """Read exact sysctl bytes, refusing missing values rather than guessing."""
    libc = ctypes.CDLL(None, use_errno=True)
    size = ctypes.c_size_t()
    if libc.sysctlbyname(name.encode(), None, ctypes.byref(size), None, 0):
        raise OSError(ctypes.get_errno(), f"sysctl {name}")
    value = ctypes.create_string_buffer(size.value)
    if libc.sysctlbyname(name.encode(), value, ctypes.byref(size), None, 0):
        raise OSError(ctypes.get_errno(), f"sysctl {name}")
    return value.raw[:size.value]


def host_identity():
    """Use machine identity, never a caller's home, hostname or repository."""
    if sys.platform == "darwin":
        class Timespec(ctypes.Structure):
            _fields_ = [("seconds", ctypes.c_long), ("nanoseconds", ctypes.c_long)]
        value = (ctypes.c_ubyte * 16)()
        timeout = Timespec(0, 0)
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.gethostuuid(value, ctypes.byref(timeout)):
            raise OSError(ctypes.get_errno(), "gethostuuid")
        return "darwin:" + str(uuid.UUID(bytes=bytes(value)))
    if sys.platform.startswith("linux"):
        value = Path("/etc/machine-id").read_text().strip()
        if len(value) != 32 or any(c not in "0123456789abcdef" for c in value):
            raise Refusal("invalid native machine identity")
        return "linux:" + value
    raise Refusal(f"unsupported host platform {sys.platform}")


def boot_identity():
    """Read a native boot UUID; elapsed time is never evidence of a reboot."""
    value = (sysctl("kern.bootsessionuuid").rstrip(b"\0").decode() if sys.platform == "darwin"
             else Path("/proc/sys/kernel/random/boot_id").read_text().strip())
    return str(uuid.UUID(value))


def bsd_info(pid):
    """Distinguish a vanished process from an unreadable process."""
    info = BsdInfo()
    lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                ctypes.c_void_p, ctypes.c_int]
    ctypes.set_errno(0)
    if lib.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) != ctypes.sizeof(info):
        error = ctypes.get_errno() or errno.EIO
        # A census member can exit between enumeration and proc_pidinfo. Darwin
        # can return an unreadable record during that transition. Only an
        # independent kernel ESRCH confirms absence; a live or denied PID still
        # fails closed with the original observation error.
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            raise ProcessLookupError(errno.ESRCH, f"PID {pid} exited during observation") from None
        except OSError:
            pass
        raise OSError(error, f"cannot observe PID {pid}")
    return info


def process(pid, boot):
    """Observe a process incarnation, ancestry and live/zombie status."""
    integer(pid, "pid", 2)
    if sys.platform == "darwin":
        info = bsd_info(pid)
        if info.pid != pid:
            raise Refusal("native PID observation mismatch")
        start, parent, state = f"macos:{info.seconds}:{info.microseconds}", info.ppid, info.status
        live = state != 5
    else:
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        except FileNotFoundError:
            raise ProcessLookupError(errno.ESRCH, f"PID {pid} exited") from None
        if len(fields) < 20:
            raise Refusal("malformed native process record")
        start, parent = f"linux:{fields[19]}", int(fields[1])
        live = fields[0] not in ("Z", "X")
    return dict(pid=pid, start=start, boot=boot, parent=parent, session=os.getsid(pid), live=live)


def identity(pid, boot):
    """Capture the stable subset of a live process observation."""
    value = process(pid, boot)
    if not value["live"]:
        raise ProcessLookupError(errno.ESRCH, f"PID {pid} is a zombie")
    return {k: value[k] for k in ("pid", "start", "boot")}


def observe(owner, boot):
    """Return True/live, False/gone or None/unknown; never equate denial with exit."""
    try:
        return owner["boot"] == boot and identity(owner["pid"], boot) == owner
    except ProcessLookupError:
        return False
    except (OSError, Refusal):
        return None


def pids():
    """Enumerate native PIDs, refusing a possibly truncated Darwin census."""
    if sys.platform != "darwin":
        return [int(p.name) for p in Path("/proc").iterdir() if p.name.isdigit()]
    lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    needed = lib.proc_listpids(1, 0, None, 0)
    if needed <= 0:
        raise OSError(ctypes.get_errno(), "proc_listpids size")
    buffer = (ctypes.c_int * (needed // 4 + 1024))()
    size = lib.proc_listpids(1, 0, buffer, ctypes.sizeof(buffer))
    if size <= 0 or size >= ctypes.sizeof(buffer):
        raise Refusal("incomplete native process census")
    return [pid for pid in buffer[:size // 4] if pid > 1]


def session_members(session):
    """Observe all surviving session PIDs, including a session without its leader."""
    members = []
    for pid in pids():
        try:
            if os.getsid(pid) == session:
                members.append(pid)
        except ProcessLookupError:
            continue
    return members


def peer_identity(connection, boot):
    """Bind socket requests to a same-account native PID, not a claimed JSON PID."""
    if sys.platform == "darwin":
        pid = struct.unpack("i", connection.getsockopt(0, 2, 4))[0]  # LOCAL_PEERPID
        uid = bsd_info(pid).uid
    else:
        pid, uid, _gid = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
    if uid != os.getuid():
        raise Refusal("foreign service account")
    return identity(pid, boot)
