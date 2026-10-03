#!/usr/bin/env python3
"""Reconcile the owned project verification window (SH-748, SH-822).

The window has a reader pane that follows the project journal (right) and,
when the daemon supplies a launch, a Verifier Agent pane (left).

Runs only as the daemon composes it: plugins/story/lib/tmux_server_env.py
and probe_budget.py followed by this file (src/daemon/activity/window.rs), which supplies
`client_environment`, `pane_overrides`, `scrub_owned_session`, `probe_run` and
`probe_operation` without copying policies.
"""

import fcntl
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

FORMAT = ("#{window_id}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@storyhook-journal}"
          "\t#{@storyhook-reader}\t#{pane_start_command}\t#{@storyhook-command}\t#{@storyhook-agent-started}")
FIELDS = 10
# Seconds before a missing agent pane is created again. A person who closes it
# gets it back; a launch that fails at once cannot become a restart loop.
AGENT_RESPAWN_COOLDOWN = 60
# The agent's $0. It is in the pane's start command from creation, so a pass
# interrupted after the split can never leave an agent this view cannot see.
AGENT_MARKER = "storyhook-verifier:"
# A person restarts the agent: the loop waits for Enter after every exit, so a
# launch that fails at once shows its status instead of being run again.
AGENT_LOOP = ('while :; do "$@"; status=$?; '
              'printf "\\n[storyhook] The Verifier Agent exited (status %s). '
              'Press Enter to start it again.\\n" "$status"; '
              'read -r _ || exit "$status"; done')

# One process performs one pass. Resolve again if a test invokes another pass,
# but never switch generations between a pass's probe and allocation.
VIEW_TARGET = None


def view_target(ensure=False):
    """Select the daemon's default logical socket before any terminal access."""
    environment = client_environment(os.environ)
    environment.pop("TMUX", None)
    environment.pop("TMUX_PANE", None)
    return resolve_target(None, environment, probe_run, environment, ensure=ensure)


def tmux(*args):
    """Use literal argv and bounded clients on the default server.

    Any of these clients may start that server, so each passes only the
    server-start allowlist from tmux_server_env, which the daemon prepends to
    this program (SH-758): the daemon's inherited environment is not the
    machine's, and a server keeps whatever its first client carried.
    """
    env = client_environment(os.environ)
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)
    target = VIEW_TARGET if VIEW_TARGET is not None else view_target()
    result = probe_run(["tmux", *target_arguments(target, args)], env=env, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"tmux {args[0]}: {result.stderr.strip()} (exit {result.returncode})")
    # Empty tab-delimited fields are identity evidence, even on the last row.
    return result.stdout.rstrip("\n")


def inventory(session):
    """One row per pane. A failed census is never evidence that a pane is absent."""
    rows = tmux("list-panes", "-s", "-t", "=" + session, "-F", FORMAT)
    return [row.split("\t") for row in rows.splitlines()]


def is_reader(row):
    """The pane the window records as its reader, by pane ID and PID."""
    return len(row) == FIELDS and row[6] == row[2] + ":" + row[3]


def reader_like(row):
    """A pane still running the recorded reader command, whatever the record says."""
    return len(row) == FIELDS and row[8] != "" and row[7] == row[8]


def is_agent(row, owner):
    """A pane this window launched as its Verifier Agent, known by its marker."""
    return len(row) == FIELDS and AGENT_MARKER + owner in row[7]


def healthy(row):
    """Reject a dead pane or a respawned command."""
    return row[4] == "0" and reader_like(row)


def mark(window, pane, owner):
    """Record ownership and the reader's identity by exact IDs, after creation."""
    reader = tmux("display-message", "-p", "-t", pane, "#{pane_id}:#{pane_pid}")
    command = tmux("display-message", "-p", "-t", pane, "#{pane_start_command}")
    tmux("set-option", "-w", "-t", window, "@storyhook-journal", owner, ";",
         "set-option", "-w", "-t", window, "@storyhook-reader", reader, ";",
         "set-option", "-w", "-t", window, "@storyhook-command", command, ";",
         "set-option", "-w", "-t", window, "automatic-rename", "off", ";",
         "set-option", "-w", "-t", window, "allow-rename", "off")


def allocate(session, name, owner, reader, new_session=False):
    """Stamp allocation ownership in one server command group, before returning."""
    args = (["new-session", "-s", session] if new_session else
            ["new-window", "-t", "=" + session + ":"])
    created = tmux(*args, "-d", "-P", "-F", "#{window_id}\t#{pane_id}", "-c", str(Path.home()),
                   "-n", name, *reader, ";", "set-option", "-w", "-t",
                   "=" + session + ":=" + name, "@storyhook-journal", owner)
    window, pane = created.splitlines()[0].split("\t")
    return window, pane


@probe_operation()
def reconcile(session, directory, binary, checkout=None, *agent):
    """Keep the owned reader and agent alive without disturbing other terminal work.

    Each pass makes at most one structural change (create the window, replace
    the reader, or create the agent), so every pass fits the one operation
    budget and a failure leaves at most one step to recover.
    """
    global VIEW_TARGET
    if os.environ.get("STORYHOOK_VERIFIER_MIRROR") == "0":
        return
    directory = Path(directory).resolve()
    # SH-771: the daemon owns the journal directory and prepares it, ignore
    # file first, before it runs this view (src/daemon/activity/window.rs).
    # A directory created here would hold .view.lock where git can see it.
    if not directory.is_dir():
        raise RuntimeError(f"journal directory {directory} is absent; the daemon prepares it first")
    owner = hashlib.sha256(os.fsencode(directory)).hexdigest()
    fd = os.open(directory / ".view.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return  # The current owner is already reconciling this project.
        VIEW_TARGET = view_target(ensure=True)
        reader = [str(Path(binary).resolve()), "daemon", "logs", "--directory", str(directory), "--follow"]
        launch = (checkout, list(agent)) if checkout and agent else None
        # new-session -A is not suitable here: it attaches when the session exists.
        sessions = tmux("list-sessions", "-F", "#{session_name}") if server_has_sessions() else ""
        if session not in sessions.splitlines():
            try:
                window, pane = allocate(session, "verification", owner, reader, new_session=True)
            except RuntimeError as error:
                if "duplicate session:" not in str(error):
                    raise
                # Another project/helper can create the session while this pass
                # holds only its own view lock. Never infer success from an
                # unrelated allocation error or a prefix-matched session.
                tmux("has-session", "-t", "=" + session)
            else:
                try:
                    mark(window, pane, owner)
                except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                    rollback(window, error)
                    raise
                return
        rows = inventory(session)
        windows = {row[0] for row in rows if row[1] == "verification"}
        if len(windows) > 1 or any(row[1] == "verification" and row[5] != owner for row in rows):
            raise RuntimeError(f"verification ownership conflict in session {session}")
        own = [row for row in rows if row[1] == "verification"]
        reader_row = next((row for row in own if is_reader(row)), None)
        if reader_row and healthy(reader_row):
            reap_temporary(session, rows, owner)
            if launch:
                ensure_agent(session, own, owner, reader_row, launch)
            return
        others = [row for row in own if row[4] == "0" and not reader_like(row)]
        if others:
            replace_reader(session, own, owner, reader, others)
            reap_temporary(session, rows, owner)
            return
        replace_window(session, own, owner, reader)
        reap_temporary(session, rows, owner)


def replace_window(session, own, owner, reader):
    """Replace a window that holds no live pane but its reader's."""
    # A period is a tmux pane delimiter even with exact window-name matching.
    # The initial ownership stamp must resolve this name before mark() runs.
    window, pane = allocate(session, "verification-pending-" + uuid.uuid4().hex, owner, reader)
    try:
        mark(window, pane, owner)
        # Recheck immutable evidence immediately before retiring the old reader.
        if own:
            current = [row for row in inventory(session) if row[0] == own[0][0]]
            if current != own:
                raise RuntimeError("verification ownership changed during replacement")
            tmux("kill-window", "-t", own[0][0])
        tmux("rename-window", "-t", window, "verification")
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        rollback(window, error)
        raise


def replace_reader(session, own, owner, reader, others):
    """Replace only the reader of a window that other live panes share.

    Killing the window would end the agent and any pane a person added, so the
    new reader is split in beside a live pane, marked, and only then are the
    old reader panes retired, each after its evidence is checked again.
    """
    window = own[0][0]
    agents = sorted((row for row in others if is_agent(row, owner)), key=pane_number)
    beside = (agents or others)[0][2]
    pane = tmux("split-window", "-d", "-h", "-t", beside, "-c", str(Path.home()),
                "-P", "-F", "#{pane_id}", *reader)
    try:
        mark(window, pane, owner)
        retired = [row for row in own if reader_like(row) or is_reader(row)]
        current = {row[2]: row for row in inventory(session) if row[0] == window}
        for row in retired:
            # The mark above rewrote the window options every row repeats, so
            # the pane's own immutable evidence is what is compared.
            now = current.get(row[2])
            if now is not None and now[2:5] + [now[7]] != row[2:5] + [row[7]]:
                raise RuntimeError("verification reader changed during replacement")
        for row in retired:
            if row[2] in current:
                tmux("kill-pane", "-t", row[2])
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        rollback_pane(pane, error)
        raise


def ensure_agent(session, own, owner, reader_row, launch):
    """Keep exactly one Verifier Agent pane left of the reader."""
    agents = sorted((row for row in own if is_agent(row, owner)), key=pane_number)
    if len(agents) > 1:
        # Two passes can both split before either sees the other's pane; the
        # lowest pane ID is the one that existed first.
        current = {row[2]: row for row in inventory(session) if row[0] == own[0][0]}
        for row in agents[1:]:
            if current.get(row[2], [None] * FIELDS)[2:4] == row[2:4]:
                tmux("kill-pane", "-t", row[2])
        return
    if agents:
        return
    started = own[0][9]
    if started.isdigit() and time.time() - int(started) < AGENT_RESPAWN_COOLDOWN:
        return
    checkout, command = launch
    # A server a provider process started can hold its session state; this
    # session is storyhook's, so its new panes are cleaned before the launch.
    scrub_owned_session(tmux, session, os.environ)
    window = own[0][0]
    tmux("set-option", "-w", "-t", window, "@storyhook-agent-started", str(int(time.time())), ";",
         "split-window", "-d", "-h", "-b", "-t", reader_row[2], "-c", checkout,
         *pane_overrides(os.environ), "/bin/sh", "-c", AGENT_LOOP, AGENT_MARKER + owner, *command)


def pane_number(row):
    """Order panes by creation: tmux numbers pane IDs upward from %0."""
    return int(row[2].lstrip("%") or 0)


def rollback(window, original):
    """Use only remaining time, keeping the original failure if cleanup also fails."""
    try:
        tmux("kill-window", "-t", window)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as cleanup:
        raise RuntimeError(f"{original}; cleanup of owned window {window} failed: {cleanup}; "
                           "a later reconcile will recover it") from original


def rollback_pane(pane, original):
    """Retire a reader pane this pass split in but could not mark or confirm."""
    try:
        tmux("kill-pane", "-t", pane)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as cleanup:
        raise RuntimeError(f"{original}; cleanup of owned pane {pane} failed: {cleanup}; "
                           "a later reconcile will recover it") from original


def reap_temporary(session, rows, owner):
    """Only retire interrupted allocations after a permanent view exists."""
    for row in rows:
        if (row[1].startswith(("verification-pending-", ".verification-")) and row[5] == owner
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
        absent = ("no sessions",) if VIEW_TARGET and VIEW_TARGET["protected"] else ("no server running", "no sessions", "No such file or directory")
        if any(text in str(error) for text in absent):
            return False
        raise


if __name__ == "__main__":
    try:
        reconcile(*sys.argv[1:])
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"verification view: {error}", file=sys.stderr)
        sys.exit(1)
