"""Preserve caller-owned exclusion through bounded session-effect children."""

import fcntl
import os
from pathlib import Path
import stat


def inherited_fds():
    """Return a live inherited workspace descriptor, rejecting stale markers."""
    value = os.environ.get("STORY_WORKSPACE_LOCK_FD")
    if value is None:
        return ()
    if not value.isdecimal() or int(value) < 3:
        raise ValueError("invalid inherited workspace descriptor")
    fd = int(value)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        raise ValueError("inherited workspace descriptor is not a regular file")
    return (fd,)


def require_workspace(common, story):
    """Require the exact held story lock before registering an adopted session."""
    descriptors = inherited_fds()
    if not descriptors:
        raise ValueError("session adoption requires workspace ownership")
    path = Path(common) / "storyhook/workspace-locks" / (story + ".lock")
    expected = os.stat(path, follow_symlinks=False)
    actual = os.fstat(descriptors[0])
    if not stat.S_ISREG(expected.st_mode) or (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino):
        raise ValueError("session adoption workspace identity changed")
    fcntl.flock(descriptors[0], fcntl.LOCK_EX | fcntl.LOCK_NB)
