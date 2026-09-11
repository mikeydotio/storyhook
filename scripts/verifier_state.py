"""Durable files and process facts shared by verifier lifecycle helpers."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


class Refusal(RuntimeError):
    """A lifecycle invariant cannot be established without risking evidence."""


def atomic(path, data):
    """Publish complete durable bytes without truncating the previous state."""
    path = Path(path)
    if path.is_symlink():
        raise Refusal(f"refusing symlink state file {path}")
    fd, tmp = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(tmp, path)
        sync_dir(path.parent)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def sync_dir(path):
    """Persist directory entries after a journaled rename or removal."""
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def save(path, value):
    """Atomically persist a JSON lifecycle record."""
    atomic(path, (json.dumps(value, sort_keys=True) + "\n").encode())


def read(path):
    """Distinguish absence from malformed or substituted ownership evidence."""
    path = Path(path)
    if path.is_symlink():
        raise Refusal(f"refusing symlink state file {path}")
    try:
        value = json.loads(path.read_text())
    except FileNotFoundError:
        return None
    except (ValueError, UnicodeError) as error:
        raise Refusal(f"unreadable lifecycle record {path}: {error}") from error
    if not isinstance(value, dict) or value.get("version") != 1:
        raise Refusal(f"unsupported lifecycle record {path}")
    return value


def paths(common, worktree):
    """Derive one physical workspace identity and permanent ownership location."""
    common = Path(common).resolve(strict=True)
    worktree = Path(worktree).absolute()
    if worktree.is_symlink() or worktree.resolve() != worktree:
        raise Refusal(f"verifier worktree path is not physical: {worktree}")
    directory = common / "storyhook/verifier-lifecycle"
    if directory.resolve() != directory:
        raise Refusal(f"verifier state is not a physical owned directory: {directory}")
    directory.mkdir(parents=True, exist_ok=True)
    key = hashlib.sha256(os.fsencode(worktree)).hexdigest()
    return common, worktree, directory / key


def boot():
    """Read a kernel boot identity, never infer reboot from elapsed time."""
    linux = Path("/proc/sys/kernel/random/boot_id")
    if linux.exists():
        return linux.read_text().strip()
    result = subprocess.run(["sysctl", "-n", "kern.bootsessionuuid"],
                            capture_output=True, text=True, check=True)
    if not result.stdout.strip():
        raise Refusal("kernel returned no boot identity")
    return result.stdout.strip()


def session_members(sid):
    """Find live session participants even when their leader has disappeared."""
    if not isinstance(sid, int) or sid <= 0:
        raise Refusal(f"invalid recorded execution session: {sid!r}")
    result = subprocess.run(["ps", "-axo", "pid=,stat="],
                            capture_output=True, text=True, check=True)
    members = []
    for line in result.stdout.splitlines():
        pid_text, state = line.split(maxsplit=1)
        pid = int(pid_text)
        if state.startswith("Z"):
            continue
        try:
            if os.getsid(pid) == sid:
                members.append(pid)
        except ProcessLookupError:
            continue
    return members


def held(common, worktree, key):
    """Validate the capability against its durable session and exact mappings."""
    owner = read(str(key) + ".owner")
    return bool(owner and owner.get("common") == str(common)
                and owner.get("worktree") == str(worktree)
                and owner.get("nonce") == os.environ.get("STORYHOOK_VERIFIER_OWNER")
                and os.getsid(0) in (owner.get("session"), owner.get("gate_session"))
                and owner.get("boot") == boot())
