#!/usr/bin/env python3
"""Reconcile the single owned project verification reader (SH-748)."""

import fcntl
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import uuid

TIMEOUT = 3
FORMAT = "#{window_id}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@storyhook-journal}\t#{@storyhook-reader}\t#{pane_start_command}\t#{@storyhook-command}"


def tmux(*args):
    """Use literal argv and bounded clients on the default server."""
    env = os.environ.copy()
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)
    result = subprocess.run(["tmux", *args], env=env, capture_output=True,
                            text=True, timeout=TIMEOUT)
    if result.returncode:
        raise RuntimeError(f"tmux {args[0]}: {result.stderr.strip()} (exit {result.returncode})")
    return result.stdout.strip()


def inventory(session):
    """A failed census is never evidence that an owned window is absent."""
    rows = tmux("list-panes", "-s", "-t", "=" + session, "-F", FORMAT)
    return [row.split("\t") for row in rows.splitlines()]


def healthy(row, owner):
    """Reject a dead pane, a respawned command, or a different reader ID."""
    return (len(row) == 9 and row[4] == "0" and row[5] == owner
            and row[6] == row[2] + ":" + row[3] and row[7] == row[8])


def mark(window, owner):
    """Record ownership only after creation returned an exact window ID."""
    reader = tmux("display-message", "-p", "-t", window, "#{pane_id}:#{pane_pid}")
    command = tmux("display-message", "-p", "-t", window, "#{pane_start_command}")
    tmux("set-option", "-w", "-t", window, "@storyhook-journal", owner)
    tmux("set-option", "-w", "-t", window, "@storyhook-reader", reader)
    tmux("set-option", "-w", "-t", window, "@storyhook-command", command)
    tmux("set-option", "-w", "-t", window, "automatic-rename", "off")
    tmux("set-option", "-w", "-t", window, "allow-rename", "off")


def allocate(session, name, owner, reader, new_session=False):
    """Stamp allocation ownership in one server command group, before returning."""
    args = (["new-session", "-s", session] if new_session else
            ["new-window", "-t", "=" + session + ":"])
    return tmux(*args, "-d", "-P", "-F", "#{window_id}", "-c", str(Path.home()),
                "-n", name, *reader, ";", "set-option", "-w", "-t",
                "=" + session + ":=" + name, "@storyhook-journal", owner)


def reconcile(session, directory, binary):
    """Keep an owned reader alive without disturbing other terminal work."""
    if os.environ.get("STORYHOOK_VERIFIER_MIRROR") == "0":
        return
    directory = Path(directory).resolve()
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    owner = hashlib.sha256(os.fsencode(directory)).hexdigest()
    fd = os.open(directory / ".view.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return  # The current owner is already reconciling this project.
        reader = [str(Path(binary).resolve()), "daemon", "logs", "--directory", str(directory), "--follow"]
        # new-session -A is not suitable here: it attaches when the session exists.
        sessions = tmux("list-sessions", "-F", "#{session_name}") if server_has_sessions() else ""
        if session not in sessions.splitlines():
            window = allocate(session, "verification", owner, reader, new_session=True)
            try:
                mark(window, owner)
            except (OSError, RuntimeError, subprocess.TimeoutExpired):
                tmux("kill-window", "-t", window)
                raise
            return
        rows = inventory(session)
        matches = [row for row in rows if row[1] == "verification"]
        if len(matches) > 1 or (matches and matches[0][5] != owner):
            raise RuntimeError(f"verification ownership conflict in session {session}")
        if matches and healthy(matches[0], owner):
            reap_temporary(session, rows, owner)
            return
        old = matches[0] if matches else None
        window = allocate(session, ".verification-" + uuid.uuid4().hex, owner, reader)
        try:
            mark(window, owner)
            # Recheck immutable evidence immediately before retiring the old reader.
            if old:
                current = [row for row in inventory(session) if row[0] == old[0]]
                if current != [old]:
                    raise RuntimeError("verification ownership changed during replacement")
                tmux("kill-window", "-t", old[0])
            tmux("rename-window", "-t", window, "verification")
        except (OSError, RuntimeError, subprocess.TimeoutExpired):
            tmux("kill-window", "-t", window)
            raise
        reap_temporary(session, rows, owner)


def reap_temporary(session, rows, owner):
    """Only retire interrupted allocations after a permanent view exists."""
    for row in rows:
        if (row[1].startswith(".verification-") and row[5] == owner
                and len([other for other in rows if other[0] == row[0]]) == 1):
            current = [other for other in inventory(session) if other[0] == row[0]]
            if current == [row]:
                tmux("kill-window", "-t", row[0])


def server_has_sessions():
    """Only tmux's documented empty-server diagnostics permit session creation."""
    try:
        tmux("list-sessions", "-F", "#{session_name}")
        return True
    except RuntimeError as error:
        if any(text in str(error) for text in ("no server running", "no sessions", "No such file or directory")):
            return False
        raise


if __name__ == "__main__":
    try:
        reconcile(*sys.argv[1:])
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"verification view: {error}", file=sys.stderr)
        sys.exit(1)
