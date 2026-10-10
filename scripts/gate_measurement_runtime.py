"""OS observations and durable logs for verifier-owned measurements."""

import json
import contextlib
import math
import ctypes
import datetime
import hashlib
import os
from pathlib import Path
import resource
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from verifier_state import Refusal


@contextlib.contextmanager
def observation_deadline(end, *, clock=None):
    """Cap every nested capture to this observation without renewing its parent.

    Collectors are sequential. Restore the exact inherited environment on all
    exits; cleanup keeps its existing independent custody allowance.
    """
    clock = time.monotonic if clock is None else clock
    name = 'STORYHOOK_MEASUREMENT_END'
    previous = os.environ.get(name)
    if type(end) not in (int, float) or not math.isfinite(end):
        raise Refusal('observation deadline must be finite')
    if previous is not None:
        try:
            parent = float(previous)
        except ValueError as error:
            raise Refusal('parent observation deadline is invalid') from error
        if not math.isfinite(parent):
            raise Refusal('parent observation deadline must be finite')
        end = min(end, parent)
    def require():
        if clock() >= end:
            raise Refusal('host observation exhausted its original allowance')
    require()
    os.environ[name] = str(end)
    try:
        yield end
        require()
    finally:
        if previous is None:
            os.environ.pop(name, None)
        else:
            os.environ[name] = previous


def journal(path, event):
    """Append and persist one complete observation without rewriting prior evidence."""
    flags = os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW
    data = (json.dumps(event, allow_nan=False, sort_keys=True) + '\n').encode()
    fd = os.open(path, flags, 0o600)
    try:
        if os.write(fd, data) != len(data):
            raise Refusal(f"short journal append: {path}")
        os.fsync(fd)
    finally:
        os.close(fd)


def records(path):
    """Read the entire journal, refusing corruption rather than skipping rows."""
    path = Path(path)
    if path.is_symlink():
        raise Refusal(f"symlink journal: {path}")
    try:
        raw = path.read_text()
    except FileNotFoundError:
        return []
    if raw and not raw.endswith('\n'):
        raise Refusal(f"incomplete journal append: {path}")
    try:
        rows = [json.loads(line) for line in raw.splitlines()]
        if any(not isinstance(row, dict) or not isinstance(row.get('kind'), str) for row in rows):
            raise ValueError('invalid event shape')
        return rows
    except (ValueError, TypeError) as error:
        raise Refusal(f"corrupt journal {path}: {error}") from error


def pending_sample(rows):
    """Refuse a restart whose last started sample has no terminal observation."""
    starts = [r['index'] for r in rows if r['kind'] == 'start']
    ends = [r['index'] for r in rows if r['kind'] == 'sample']
    if (len(set(starts)) != len(starts) or len(set(ends)) != len(ends)
            or any(i not in starts for i in ends)):
        raise Refusal('journal has duplicate starts or samples without a start')
    return starts != ends


def mirror_progress(source, destination, offset):
    """Copy each completed event once; a partial append waits for its next read."""
    if not Path(source).exists():
        return offset
    with open(source, 'rb') as stream:
        if os.fstat(stream.fileno()).st_size < offset:
            raise Refusal('gate progress journal shrank')
        stream.seek(offset)
        raw = stream.read()
    length = raw.rfind(b'\n') + 1
    complete = raw[:length]
    for line in complete.splitlines():
        json.loads(line)
    if complete:
        fd = os.open(destination, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        try:
            if os.write(fd, complete) != len(complete):
                raise Refusal('short progress mirror append')
        finally:
            os.close(fd)
    return offset + length


def execution_active(path):
    """Recognize actual Rust test cases under a currently running gate leg."""
    states = {}
    seen = set()
    # A writer may currently be appending the last line. Completed lines are
    # sufficient for a readiness observation; the durable sample log is strict.
    try:
        raw = Path(path).read_text()
    except FileNotFoundError:
        return False
    for line in raw.splitlines(keepends=True):
        if not line.endswith('\n'):
            break
        try:
            row = json.loads(line)
        except ValueError as error:
            raise Refusal(f"corrupt gate progress: {path}") from error
        name = row.get('path')
        if row.get('kind') == 'item':
            states[name] = row.get('status')
        if row.get('kind') == 'case' and row.get('outcome') in ('pass', 'fail'):
            seen.add(name)
    return any(states.get(name) == 'running' for name in seen)


def probe_environment(base, root, parent):
    """Isolate probe state and remove dispatch, gate and measurement authority."""
    root = Path(root)
    env = {k: v for k, v in base.items() if not k.startswith(('STORYHOOK_', 'GIT_'))}
    env.update({'PATH': str(root / 'bin') + ':' + base.get('PATH', os.defpath),
                'XDG_DATA_HOME': str(root / 'data'), 'XDG_STATE_HOME': str(root / 'state'),
                'XDG_CONFIG_HOME': str(root / 'config'),
                'STORYHOOK_STORE_PATH': str(root / 'store.db'),
                'STORYHOOK_DAEMON_ADDR': '127.0.0.1:0',
                'STORYHOOK_PARENT_PID': str(parent), 'STORYHOOK_ALLOW_TEMP_PROJECT': '1',
                'STORYHOOK_ALLOW_UNINSTALLED_DAEMON': '1',
                'STORYHOOK_ALLOW_UNINSTALLED_MIGRATION': '1'})
    return env


def sha256(path):
    """Hash immutable binary/hook identities without loading binaries into memory."""
    with open(path, 'rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def capture(argv, **kwargs):
    """Capture successful OS observations with command and stderr on failure."""
    from gate_measurement_command import bounded
    root = os.environ.get('STORYHOOK_MEASUREMENT_OPERATIONS')
    if root is None:
        root = str(Path(tempfile.mkdtemp(prefix='storyhook-measurement-command-')).resolve())
    seconds = 30
    if 'STORYHOOK_MEASUREMENT_END' in os.environ:
        seconds = min(seconds, float(os.environ['STORYHOOK_MEASUREMENT_END']) - time.monotonic())
        if seconds <= 0:
            raise Refusal('measurement campaign allowance expired')
    result = bounded(argv, root=root, seconds=seconds, **kwargs)
    if result.returncode:
        raise Refusal(f"{argv!r} exited {result.returncode}: {result.stderr.strip()}")
    return result.stdout.strip()


def scheduling():
    """Read actual macOS QoS, darwin-BG and nice before interpreting timings."""
    if sys.platform != 'darwin':
        raise Refusal('gate-class measurement collection currently requires macOS')
    libc = ctypes.CDLL(None, use_errno=True)
    libc.qos_class_self.restype = ctypes.c_uint
    libc.getpriority.argtypes = [ctypes.c_int, ctypes.c_uint]
    libc.getpriority.restype = ctypes.c_int
    ctypes.set_errno(0)
    background = libc.getpriority(4, 0)
    if ctypes.get_errno():
        raise OSError(ctypes.get_errno(), 'read PRIO_DARWIN_PROCESS')
    return {'qos': libc.qos_class_self(), 'darwin_background': background,
            'nice': os.getpriority(os.PRIO_PROCESS, 0)}


def normal_class(observed):
    """An inherited background/utility clamp or niceness invalidates the control."""
    return (observed['qos'] not in (0x09, 0x11)
            and observed['darwin_background'] == 0 and observed['nice'] == 0)


def resource_limits():
    """Observe every supported soft/hard limit without changing process policy."""
    result = {}
    for name in sorted(n for n in dir(resource) if n.startswith('RLIMIT_')):
        soft, hard = resource.getrlimit(getattr(resource, name))
        result[name] = {'soft': soft, 'hard': hard}
    return result


def require_resource_limits(identity):
    """Reject missing or changed limits before interpreting a gate comparison."""
    observed = resource_limits()
    expected = identity.get('resource_limits')
    if expected != observed:
        raise Refusal(f'measurement resource limits changed or are missing: expected={expected!r}; observed={observed!r}')
    return observed


def pressure(*, include_processes=True):
    """Capture read-only host pressure and process activity with no command arguments."""
    from gate_measurement_exposure import snapshot
    observed = {**snapshot(),
            'at': datetime.datetime.now().astimezone().isoformat(),
            'load': list(os.getloadavg()), 'cores': os.cpu_count(),
            'memory': capture(['/usr/bin/memory_pressure', '-Q'])}
    if include_processes:
        observed['processes'] = capture(['ps', '-axo', 'pid=,ppid=,pcpu=,comm='])
        observed['resource_processes'] = capture(['ps', '-axo', 'pid=,ppid=,pcpu=,rss=,comm='])
    return observed
