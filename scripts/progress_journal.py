"""Bounded complete-record observations for the shell gate watchdog."""

import json
import os
import stat
import sys

# This is a protocol/read bound, not a workload timing allowance. Keep the
# same record limit as the Rust IdleDeadline observer.
MAX_RECORD = 1_048_576


def observe(path, cursor=None):
    """Return the next identity/extent/cursor and whether real progress was appended."""
    if cursor is not None and (not isinstance(cursor, list) or len(cursor) != 4
                              or any(type(v) is not int or v < 0 for v in cursor)
                              or cursor[3] > cursor[2]):
        raise ValueError("invalid journal observation cursor")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise ValueError("journal is not a regular file")
        state = [info.st_dev, info.st_ino, info.st_size, info.st_size]
        if cursor is None:
            return state, False
        if state[:2] != cursor[:2]:
            raise ValueError("journal was replaced during verification")
        if state[2] < cursor[2]:
            raise ValueError("journal shrank during verification")
        state[3] = cursor[3]
        os.lseek(fd, state[3], os.SEEK_SET)
        data = os.read(fd, min(state[2] - state[3], MAX_RECORD + 1))
        lines = data.split(b"\n")
        if len(lines[-1]) > MAX_RECORD:
            raise ValueError("journal contains an oversized record")
        changed = False
        for line in lines[:-1]:
            if len(line) + 1 > MAX_RECORD:
                raise ValueError("journal contains an oversized record")
            state[3] += len(line) + 1
            try:
                row = json.loads(line)
            except (ValueError, UnicodeError):
                row = None  # Complete legacy text lines remain progress.
            if not isinstance(row, dict) or row.get("kind") != "resource":
                changed = True
        return state, changed
    finally:
        os.close(fd)


def main(arguments):
    """Emit a compact cursor and boolean for the shell, without evaluating shell text."""
    if len(arguments) not in (1, 2):
        print("usage: progress_journal.py <journal> [cursor]", file=sys.stderr)
        return 2
    try:
        cursor, changed = observe(arguments[0], json.loads(arguments[1]) if len(arguments) == 2 else None)
        print(json.dumps(cursor, separators=(",", ":")), int(changed))
        return 0
    except (OSError, ValueError) as error:
        print(f"progress-journal: {arguments[0]}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
