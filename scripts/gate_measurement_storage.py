"""Private measurement storage and pressure admission; never a cache sweeper."""

import os
from pathlib import Path
import shutil
import stat
import sys

from verifier_state import Refusal

GIB = 1024 ** 3
INITIAL_FREE = 130 * GIB
SYSTEM_HEADROOM = 40 * GIB
TARGET_CAP = 40 * GIB
EVIDENCE_CAP = 10 * GIB


def directory_identity(path):
    path = Path(path)
    if not path.is_absolute() or path.resolve() != path:
        raise Refusal(f"measurement directory is not physical: {path}")
    st = path.lstat()
    if not stat.S_ISDIR(st.st_mode):
        raise Refusal(f"measurement directory is not a directory: {path}")
    return {"path": str(path), "device": st.st_dev, "inode": st.st_ino}


def check_identity(expected):
    if directory_identity(expected['path']) != expected:
        raise Refusal(f"measurement directory identity changed: {expected['path']}")


def usage(path, *, excluded=(), allow_links=False, allow_sockets=False):
    """Measure allocated bytes without following substituted filesystem entries."""
    total = 0
    pending = [Path(path)]
    while pending:
        parent = pending.pop()
        for item in os.scandir(parent):
            child = Path(item.path)
            if child in excluded:
                continue
            st = item.stat(follow_symlinks=False)
            if allow_links and stat.S_ISLNK(st.st_mode):
                total += st.st_blocks * 512
                continue
            if allow_sockets and stat.S_ISSOCK(st.st_mode):
                continue
            if stat.S_ISLNK(st.st_mode):
                raise Refusal(f"symlink in measurement storage: {child}")
            if stat.S_ISDIR(st.st_mode):
                pending.append(child)
            elif not stat.S_ISREG(st.st_mode):
                raise Refusal(f"nonregular measurement storage entry: {child}")
            total += st.st_blocks * 512
    return total


def reserve_description(output):
    """Create one fresh exact-owned target. This describes, but does not reserve, disk."""
    output = Path(output)
    target = output / 'targets' / 'class'
    target.parent.mkdir(mode=0o700, exist_ok=True)
    target.mkdir(mode=0o700)  # existing/shared target is never adopted
    return {'version': 1, 'root': directory_identity(output),
            'targets': [directory_identity(target)], 'disposable': False,
            'initial_free_required': INITIAL_FREE, 'headroom': SYSTEM_HEADROOM,
            'target_cap': TARGET_CAP, 'evidence_cap': EVIDENCE_CAP}


def check_storage(description, *, initial=False, disk=shutil.disk_usage, size=usage):
    """Fail closed on low space, growth, sensor failure or target substitution."""
    if (description.get('version') != 1 or description.get('headroom') != SYSTEM_HEADROOM
            or description.get('initial_free_required') != INITIAL_FREE
            or description.get('target_cap') != TARGET_CAP
            or description.get('evidence_cap') != EVIDENCE_CAP):
        raise Refusal('measurement storage policy is missing or changed')
    check_identity(description['root'])
    root = Path(description['root']['path'])
    targets = description['targets']
    if not 1 <= len(targets) <= 2 or len({x['path'] for x in targets}) != len(targets):
        raise Refusal('measurement requires at most two distinct exact-owned targets')
    sizes = {}
    for target in targets:
        path = Path(target['path'])
        if path.parent != root / 'targets':
            raise Refusal('measurement target is shared or outside the owned target namespace')
        check_identity(target)
        sizes[str(path)] = size(path)
        if sizes[str(path)] > TARGET_CAP:
            raise Refusal(f'measurement target exceeded 40 GiB: {path}')
    # Count source, probe state and logs, without following source/probe links.
    # Live Unix sockets contribute no allocated file bytes and are never removed.
    evidence = size(root, excluded=tuple(Path(x['path']) for x in targets),
                    allow_links=True, allow_sockets=True)
    if evidence > EVIDENCE_CAP:
        raise Refusal('measurement evidence exceeded 10 GiB')
    free = disk(root).free
    if type(free) is not int or free < (INITIAL_FREE if initial else SYSTEM_HEADROOM):
        raise Refusal('measurement free-space allowance is unavailable or exhausted')
    return {'free_bytes': free, 'target_bytes': sizes, 'evidence_bytes': evidence,
            'disk_reserved': False}


def pressure_level(read=None):
    """Require the native normal-pressure flag; unknown and warning both stop work."""
    if read is None:
        from host_admission.native import sysctl
        read = lambda: int.from_bytes(sysctl('kern.memorystatus_vm_pressure_level'), sys.byteorder)
    value = read()
    if type(value) is not int or value != 1:
        raise Refusal(f'measurement requires known normal native memory pressure: {value!r}')
    return value
