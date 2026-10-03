"""A permanent local namespace; no home or repository can create another host cap."""

import fcntl
import os
from pathlib import Path
import stat

from .policy import Refusal

ROOT = Path("/var/tmp/storyhook-host-admission-v1")


def directory(path, *, create=False):
    """Require an owned private directory and reject a substituted final symlink."""
    path = Path(path)
    if path.is_symlink():
        raise Refusal(f"symlink authority root: {path}")
    path = path.resolve()
    if create:
        path.mkdir(mode=0o700, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise Refusal(f"authority root is not a private directory owned by this account: {path}")
    return path


def check_file(path, *, socket=False):
    """Validate a broker-owned regular file or socket without following links."""
    info = Path(path).lstat()
    kind = stat.S_ISSOCK if socket else stat.S_ISREG
    if not kind(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise Refusal(f"unsafe authority entry: {path}")
    return info


def open_private(path, *, create=False):
    """Open a private permanent inode; symlink and hard-link aliases are refused."""
    flags = os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC
    if create:
        flags |= os.O_CREAT
    try:
        fd = os.open(path, flags, 0o600)
    except FileNotFoundError:
        raise
    except OSError as error:
        raise Refusal(f"cannot open private authority file {path}: {error}") from error
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_nlink != 1:
            raise Refusal(f"unsafe authority file: {path}")
        if Path(path).stat().st_ino != info.st_ino:
            raise Refusal(f"authority inode changed: {path}")
        return fd
    except BaseException:
        os.close(fd)
        raise


def exclusive(path):
    """Take the permanent broker lock; never replace or unlink it on contention."""
    fd = open_private(path, create=True)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return fd
    except BlockingIOError:
        os.close(fd)
        raise Refusal("a host admission broker already owns this namespace") from None
    except BaseException:
        os.close(fd)
        raise
