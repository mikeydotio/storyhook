#!/usr/bin/env python3
"""Reserve a checkout-local build number and hold ownership through a command.

The lock lives outside BUILD: replacing the counter must not replace the inode
other builders lock. Children inherit ownership to fence a killed controller.
"""

import argparse
import fcntl
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile

MAX_NUMBER = (1 << 64) - 1


def read_number(path):
    """Read canonical persisted state; missing or unsafe state is an error."""
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError(f"{path}: expected a regular file")
    raw = path.read_bytes()
    if not re.fullmatch(rb"(?:0|[1-9][0-9]{0,19})\n", raw):
        raise ValueError(f"{path}: expected canonical unsigned integer and newline")
    number = int(raw)
    if number > MAX_NUMBER:
        raise ValueError(f"{path}: unsigned 64-bit overflow")
    return number


def write_number(path, number):
    """Publish durable counter state through a same-directory atomic rename."""
    if not path.stat().st_mode & 0o222 or not os.access(path, os.W_OK):
        raise PermissionError(f"{path}: counter is not writable")
    descriptor, name = tempfile.mkstemp(prefix=".BUILD-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w") as output:
            os.fchmod(output.fileno(), stat.S_IMODE(path.stat().st_mode))
            output.write(f"{number}\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(name, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def verify_lock(root):
    """Require the inherited open file description that owns this counter."""
    descriptor = int(os.environ.get("STORYHOOK_BUILD_LOCK_FD", "-1"))
    held = os.fstat(descriptor)
    lock_path = root / ".build-number.lock"
    expected = lock_path.stat()
    if (held.st_dev, held.st_ino) != (expected.st_dev, expected.st_ino):
        raise ValueError(f"{lock_path}: inherited lock names a different file")
    with lock_path.open("r+") as probe:
        try:
            fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            # Only the inherited description may already own this lock.
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return
        raise ValueError(f"{lock_path}: inherited descriptor does not hold the lock")


def main():
    """Allocate once, or consume an explicit still-current reservation."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--reserve", action="store_true")
    modes.add_argument("--check", action="store_true")
    modes.add_argument("--check-lock", action="store_true")
    modes.add_argument("--number", type=int)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if (args.reserve or args.check or args.check_lock) == bool(command):
        parser.error("use --reserve/--check alone, or supply a command after --")
    root = args.root.resolve()
    path = root / "BUILD"
    try:
        # Checks are read-only, including no creation of a lock file.
        if args.check:
            print(read_number(path))
            return 0
        if args.check_lock:
            verify_lock(root)
            print(read_number(path))
            return 0
        descriptor = os.open(root / ".build-number.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, "r+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            number = read_number(path)
            if args.number is not None:
                if args.number != number:
                    raise ValueError(f"{path}: reserved {args.number}, current {number}; refusing stale build")
            else:
                if number == MAX_NUMBER:
                    raise ValueError(f"{path}: cannot increment unsigned 64-bit maximum")
                number += 1
                write_number(path, number)
            if args.reserve:
                print(number)
                return 0
            environment = dict(os.environ, STORYHOOK_BUILD_LOCK_FD=str(lock.fileno()))
            result = subprocess.run(command, cwd=root, env=environment, pass_fds=(lock.fileno(),))
            return result.returncode if result.returncode >= 0 else 128 - result.returncode
    except (OSError, ValueError) as error:
        print(f"build-number: {path}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
