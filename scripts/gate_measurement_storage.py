"""Private measurement storage and pressure admission; never a cache sweeper."""

import errno
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


def usage(path, *, excluded=(), allow_links=False, allow_sockets=False,
          live=False, expected=None):
    """Observe allocated bytes; live descendants may vanish, admitted roots may not.

    This is not an atomic snapshot. Settled disposal scans remain strict. Every
    directory is opened relative to its anchored parent, without following links.
    """
    path = Path(path)
    admitted = directory_identity(path) if expected is None else expected
    if admitted['path'] != str(path):
        raise Refusal(f"measurement directory admission differs: {path}")
    check_identity(admitted)
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC

    def same_directory(observed, device, inode, child):
        if (not stat.S_ISDIR(observed.st_mode)
                or (observed.st_dev, observed.st_ino) != (device, inode)):
            raise Refusal(f"measurement directory identity changed: {child}")

    def walk(fd, parent):
        total = 0
        with os.scandir(fd) as scan:
            entries = list(scan)
        for item in entries:
            child = parent / item.name
            if child in excluded:
                continue
            try:
                observed = os.stat(item.name, dir_fd=fd, follow_symlinks=False)
            except FileNotFoundError as error:
                if not live or error.errno != errno.ENOENT:
                    raise
                continue
            if allow_links and stat.S_ISLNK(observed.st_mode):
                total += observed.st_blocks * 512
                continue
            if allow_sockets and stat.S_ISSOCK(observed.st_mode):
                continue
            if stat.S_ISLNK(observed.st_mode):
                raise Refusal(f"symlink in measurement storage: {child}")
            if stat.S_ISDIR(observed.st_mode):
                try:
                    child_fd = os.open(item.name, flags, dir_fd=fd)
                    try:
                        same_directory(os.fstat(child_fd), observed.st_dev,
                                       observed.st_ino, child)
                        child_bytes = walk(child_fd, child)
                        same_directory(os.stat(item.name, dir_fd=fd, follow_symlinks=False),
                                       observed.st_dev, observed.st_ino, child)
                    finally:
                        os.close(child_fd)
                except FileNotFoundError as error:
                    if not live or error.errno != errno.ENOENT:
                        raise
                    # A failed descendant scan is benign only if that directory
                    # really vanished; an unexplained sensor failure stays loud.
                    try:
                        os.stat(item.name, dir_fd=fd, follow_symlinks=False)
                    except FileNotFoundError as missing:
                        if missing.errno != errno.ENOENT:
                            raise
                        continue
                    raise
                total += child_bytes
            elif not stat.S_ISREG(observed.st_mode):
                raise Refusal(f"nonregular measurement storage entry: {child}")
            total += observed.st_blocks * 512
        return total

    root_fd = os.open(path, flags)
    try:
        same_directory(os.fstat(root_fd), admitted['device'], admitted['inode'], path)
        total = walk(root_fd, path)
        check_identity(admitted)
        return total
    finally:
        os.close(root_fd)


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
        sizes[str(path)] = size(path, live=True, expected=target)
        if type(sizes[str(path)]) is not int or sizes[str(path)] < 0:
            raise Refusal('measurement target size observation is invalid')
        if sizes[str(path)] > TARGET_CAP:
            raise Refusal(f'measurement target exceeded 40 GiB: {path}')
    # Count source, probe state and logs, without following source/probe links.
    # Live Unix sockets contribute no allocated file bytes and are never removed.
    evidence = size(root, excluded=tuple(Path(x['path']) for x in targets),
                    allow_links=True, allow_sockets=True, live=True,
                    expected=description['root'])
    check_identity(description['root'])
    for target in targets:
        check_identity(target)
    if type(evidence) is not int or evidence < 0:
        raise Refusal('measurement evidence size observation is invalid')
    if evidence > EVIDENCE_CAP:
        raise Refusal('measurement evidence exceeded 10 GiB')
    free = disk(root).free
    # Preserve room for all remaining permitted growth, not just today's usage.
    # This is an observed allowance, never an exclusive reservation or quota.
    remaining_growth = sum(TARGET_CAP - n for n in sizes.values()) + EVIDENCE_CAP - evidence
    required = max(INITIAL_FREE if initial else 0, SYSTEM_HEADROOM + remaining_growth)
    if type(free) is not int or free < required:
        raise Refusal('measurement free-space allowance is unavailable or exhausted')
    return {'free_bytes': free, 'target_bytes': sizes, 'evidence_bytes': evidence,
            'remaining_growth_bytes': remaining_growth, 'required_free_bytes': required,
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
