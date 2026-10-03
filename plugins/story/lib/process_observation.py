"""Read-only native argv, ancestry and cwd evidence for restored processes."""

import ctypes
import errno
import os
from pathlib import Path
import struct
import sys

from process_identity import BsdInfo, process_identity

MAX_ARGUMENT_BYTES = 4 * 1024 * 1024


def darwin_argv(raw):
    """Decode only argc arguments; never decode or return the following environment."""
    if len(raw) < 4:
        raise ValueError('truncated process argument header')
    count = struct.unpack_from('=i', raw)[0]
    offset = raw.find(b'\0', 4)
    if count <= 0 or count > len(raw) or offset < 0:
        raise ValueError('invalid process argument header')
    while offset < len(raw) and raw[offset] == 0:
        offset += 1
    args = []
    for _ in range(count):
        end = raw.find(b'\0', offset)
        if end < 0:
            raise ValueError('truncated process argument')
        args.append(os.fsdecode(raw[offset:end]))
        offset = end + 1
    return args


def _darwin_details(pid):
    """Use libproc and sysctl layouts published by the macOS kernel."""
    proc = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    proc.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                 ctypes.c_void_p, ctypes.c_int]
    info = BsdInfo()
    if proc.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) != ctypes.sizeof(info):
        raise OSError(ctypes.get_errno(), f'cannot read process {pid} parent')
    # proc_vnodepathinfo contains two vnode_info_path structures. vnode_info
    # occupies 152 bytes; each path has MAXPATHLEN (1024) bytes.
    paths = ctypes.create_string_buffer(2 * (152 + 1024))
    if proc.proc_pidinfo(pid, 9, 0, paths, len(paths)) != len(paths):
        raise OSError(ctypes.get_errno(), f'cannot read process {pid} cwd')
    cwd = os.fsdecode(paths.raw[152:1176].split(b'\0', 1)[0])
    libc = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
    libc.sysctl.argtypes = [ctypes.POINTER(ctypes.c_int), ctypes.c_uint,
                           ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t),
                           ctypes.c_void_p, ctypes.c_size_t]
    size = ctypes.c_size_t(ctypes.sizeof(ctypes.c_int))
    maximum = ctypes.c_int()
    if libc.sysctl((ctypes.c_int * 2)(1, 8), 2, ctypes.byref(maximum), ctypes.byref(size), None, 0):
        raise OSError(ctypes.get_errno(), 'cannot read kernel argument limit')
    if not 0 < maximum.value <= MAX_ARGUMENT_BYTES:
        raise ValueError('unsupported kernel argument limit')
    size = ctypes.c_size_t(maximum.value)
    raw = ctypes.create_string_buffer(size.value)
    if libc.sysctl((ctypes.c_int * 3)(1, 49, pid), 3, raw, ctypes.byref(size), None, 0):
        number = ctypes.get_errno()
        if number in (errno.EINVAL, errno.ESRCH):
            raise ProcessLookupError(errno.ESRCH, f'process {pid} exited while reading arguments')
        raise OSError(number, f'cannot read process {pid} arguments')
    return info.ppid, cwd, darwin_argv(raw.raw[:size.value])


def _details(pid):
    """Return parent, cwd and exact argv without a formatted ps command string."""
    if sys.platform == 'darwin':
        return _darwin_details(pid)
    if sys.platform.startswith('linux'):
        root = Path('/proc') / str(pid)
        try:
            fields = (root / 'stat').read_text().rsplit(')', 1)[1].split()
            parent = int(fields[1])
            cwd = os.readlink(root / 'cwd')
            with (root / 'cmdline').open('rb') as stream:
                raw = stream.read(MAX_ARGUMENT_BYTES + 1)
        except FileNotFoundError:
            raise ProcessLookupError(errno.ESRCH, f'process {pid} exited during observation') from None
        if not raw or len(raw) > MAX_ARGUMENT_BYTES or not raw.endswith(b'\0'):
            raise ValueError(f'process {pid} has incomplete or oversized arguments')
        return parent, cwd, [os.fsdecode(arg) for arg in raw[:-1].split(b'\0')]
    raise OSError(f'process observation is unsupported on {sys.platform}')


def observe_process(pid):
    """Bracket argv, ancestry and cwd with an unchanged kernel process identity."""
    before = process_identity(pid)
    parent, cwd, args = _details(pid)
    after = process_identity(pid)
    if before != after:
        raise RuntimeError(f'process {pid} changed during observation')
    if not os.path.isabs(cwd) or not args or parent < 0:
        raise ValueError(f'process {pid} returned incomplete ownership evidence')
    return dict(process=before, parent=parent, cwd=os.path.realpath(cwd), argv=args)


def descendants(parents, root):
    """Compute the process-tree closure without including unrelated cycles."""
    found = {root} if root in parents else set()
    while True:
        expanded = found | {pid for pid, parent in parents.items() if parent in found}
        if expanded == found:
            return found
        found = expanded
