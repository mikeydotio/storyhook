#!/usr/bin/env python3
"""Snapshot names and bytes in an owned fallback tree, without following links."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import sys


def snapshot(root):
    result = {}

    def visit(path):
        before = path.lstat()
        name = str(path.relative_to(root))
        if stat.S_ISLNK(before.st_mode):
            result[name] = ["symlink", os.readlink(path)]
        elif stat.S_ISDIR(before.st_mode):
            result[name] = ["directory"]
            for child in sorted(path.iterdir()):
                visit(child)
        elif stat.S_ISREG(before.st_mode):
            digest = hashlib.sha256()
            with path.open("rb") as source:
                for block in iter(lambda: source.read(65536), b""):
                    digest.update(block)
                after = os.fstat(source.fileno())
            identity = lambda value: (value.st_dev, value.st_ino, value.st_size,
                                      value.st_mtime_ns, value.st_ctime_ns)
            if identity(before) != identity(after):
                raise ValueError("owned fallback file changed during snapshot")
            result[name] = ["file", before.st_size, digest.hexdigest()]
        else:
            raise ValueError("unexpected special file in owned fallback tree")

    visit(root)
    return result


def require_settled(home):
    """Only observe locks and identities beneath this child's owned home."""
    sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts"))
    from host_admission import native
    from host_admission.policy import Refusal
    boot = native.boot_identity()
    locks = []
    paths = sorted(home.rglob("daemon.pid"))
    if not paths:
        raise ValueError("no owned daemon identity remains to verify settlement")
    try:
        for path in paths:
            before = path.lstat()
            if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
                raise ValueError("owned daemon pidfile is not an unshared regular file")
            source = path.open("r")
            locks.append(source)
            held = os.fstat(source.fileno())
            if (before.st_dev, before.st_ino) != (held.st_dev, held.st_ino):
                raise ValueError("owned daemon pidfile identity changed")
            fcntl.flock(source, fcntl.LOCK_EX | fcntl.LOCK_NB)
            record = json.load(source)
            pid, start = record["pid"], record["start_time"]
            if type(pid) is not int or pid <= 1 or not isinstance(start, str) or not start:
                raise ValueError("owned daemon lacks an exact native identity")
            try:
                current = native.process(pid, boot)
            except ProcessLookupError:
                continue
            except (OSError, Refusal) as error:
                raise ValueError("owned daemon settlement is unobservable") from error
            if current["start"] == start and current["live"]:
                raise ValueError("owned daemon incarnation still lives after stop")
        # Locks stay held together through the complete observation. A changed
        # PID incarnation is not signalled; only the recorded owner is relevant.
    finally:
        for source in locks:
            source.close()


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] not in ("snapshot", "settled"):
        raise SystemExit("usage: data-home-snapshot.py snapshot|settled OWNED_ROOT")
    root = Path(sys.argv[2])
    if sys.argv[1] == "snapshot":
        print(json.dumps(snapshot(root), sort_keys=True))
    else:
        require_settled(root)
