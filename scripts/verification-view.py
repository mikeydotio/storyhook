#!/usr/bin/env python3
"""Reconcile the owned project verification windows (SH-748, SH-822, SH-861).

The verification window follows the project journal; the verifier window
holds the Verifier Agent. Both are detached, single-pane allocations.

Runs only as the daemon composes it: plugins/story/lib/tmux_server_env.py
and probe_budget.py followed by this file (src/daemon/activity/window.rs), which supplies
`client_environment`, `pane_overrides`, `scrub_owned_session`, `probe_run` and
`probe_operation` without copying policies.
"""

import fcntl
import hashlib
import json
import shlex
import shutil
from process_observation import observe_process
from process_identity import process_identity
from restored_dispatch import restored_launch, live_provider
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

FORMAT = ("#{window_id}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@storyhook-journal}"
          "\t#{@storyhook-reader}\t#{pane_start_command}\t#{@storyhook-command}\t#{@storyhook-agent-started}")
FIELDS = 10
RESTORED_READERS = {}
RESTORED_AGENTS = set()
READER_PROOF = "@storyhook-reader-proof-v1"
# Seconds before a missing agent pane is created again. A person who closes it
# gets it back; a launch that fails at once cannot become a restart loop.
AGENT_RESPAWN_COOLDOWN = 60
# The agent's $0. It is in the pane's start command from creation, so a pass
# interrupted after allocation can never leave an agent this view cannot see.
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
    return len(row) == FIELDS and (row[2] in RESTORED_READERS or row[8] != "" and row[7] == row[8])


def is_agent(row, owner):
    """A pane this window launched as its Verifier Agent, known by its marker."""
    return len(row) == FIELDS and (row[2] in RESTORED_AGENTS or AGENT_MARKER + owner in row[7])


def healthy(row):
    """Reject a dead pane or a respawned command."""
    return row[4] == "0" and reader_like(row) and RESTORED_READERS.get(row[2], True)


def mark(window, pane, owner):
    """Record ownership and the reader's identity by exact IDs, after creation."""
    reader = tmux("display-message", "-p", "-t", pane, "#{pane_id}:#{pane_pid}")
    command = tmux("display-message", "-p", "-t", pane, "#{pane_start_command}")
    native = observe_process(int(reader.split(':')[1]))
    # A pane-local witness survives snapshotting; a window's numeric marker
    # alone cannot identify which restored UUID was its reader.
    tmux('set-option', '-p', '-t', pane, READER_PROOF, json.dumps(dict(owner=owner, process=native)))
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


def readopt_view(rows, owner):
    """Join preserved viewer ownership to mapped UUIDs and exact native processes."""
    RESTORED_READERS.clear()
    RESTORED_AGENTS.clear()
    evidence = restore_evidence(VIEW_TARGET, os.environ)
    if evidence is None:
        return False
    parents_result = probe_run(['ps', '-axo', 'pid=,ppid='], capture_output=True, text=True)
    if parents_result.returncode:
        raise RuntimeError('verification restoration process census: ' + parents_result.stderr)
    parents = {}
    for line in parents_result.stdout.splitlines():
        parts = line.split()
        if len(parts) != 2 or not all(p.isdecimal() for p in parts):
            raise RuntimeError('verification restoration has invalid ancestry')
        pid, parent = map(int, parts)
        if pid in parents:
            raise RuntimeError('verification restoration has duplicate processes')
        parents[pid] = parent
    live = {row[2]: row for row in rows}
    uuid_owners = {}
    for line in tmux('list-panes', '-a', '-F', '#{pane_id}\t#{@revivify-uuid}').splitlines():
        parts = line.split('\t')
        if len(parts) != 2:
            raise RuntimeError('invalid verification UUID inventory')
        if parts[1]:
            uuid_owners.setdefault(parts[1], set()).add(parts[0])
    readers = []
    for uuid_key, saved in evidence['panes'].items():
        if saved['window']['options'].get('@storyhook-journal') != owner:
            continue
        row = live.get(saved['pane_id'])
        if row is None and not uuid_owners.get(uuid_key):
            continue  # A later owned replacement already retired this UUID.
        if (row is not None and row[5] == "" and row[1].startswith("verification-retained-")
                and uuid_owners.get(uuid_key) == {row[2]}
                and tmux("show-option", "-w", "-q", "-v", "-t", row[0],
                         "@storyhook-view-released") == owner):
            continue  # This reconciler deliberately handed the mixed window back.
        if row is None or row[5] != owner or uuid_owners.get(uuid_key) != {row[2]}:
            raise RuntimeError('restored verification pane is moved, duplicated or foreign')
        if tmux('show-options', '-p', '-v', '-t', row[2], '@revivify-uuid') != uuid_key:
            raise RuntimeError('restored verification pane UUID changed')
        restored_launch(VIEW_TARGET, uuid_key, row[7])
        proof = saved['pane']['options'].get(READER_PROOF)
        if proof:
            proof = json.loads(proof)
            if proof.get('owner') != owner:
                raise RuntimeError('restored reader owner changed')
            # RV-10 does not relaunch generic reader commands. Only a currently
            # running exact reader can be retained; a replay shell is stale.
            root = observe_process(int(row[3])) if row[4] == '0' else None
            old = proof['process']
            healthy_reader = root is not None and (root['argv'], root['cwd'], root['process']['executable']) == (old['argv'], old['cwd'], old['process']['executable'])
            RESTORED_READERS[row[2]] = healthy_reader
            if healthy_reader:
                readers.append((row, root))
        agent = saved['pane'].get('agent')
        if agent:
            provider = agent.get('kind')
            launch = shutil.which(provider) if provider in ('claude', 'codex') else None
            if not launch or not agent.get('session_id'):
                raise RuntimeError('restored verifier has no exact provider conversation')
            root = process_identity(int(row[3]))
            source = dict(provider=provider, session_id=agent['session_id'],
                          provider_process=dict(executable=os.path.realpath(launch)),
                          lease=dict(worktree_path=agent['resume_cwd']))
            live_provider(source, root, parents, launch)
            RESTORED_AGENTS.add(row[2])
    if len(readers) > 1:
        raise RuntimeError('multiple healthy restored verification readers')
    if readers:
        row, native = readers[0]
        if observe_process(int(row[3])) != native:
            raise RuntimeError('restored reader changed before publication')
        if not is_reader(row) or row[7] != row[8]:
            mark(row[0], row[2], owner)
            return True
    return False


@probe_operation()
def reconcile(session, directory, binary, checkout=None, *agent):
    """Keep the owned reader and agent alive without disturbing other terminal work.

    Each pass makes at most one structural change (create the window, replace
    the reader, migrate a pane, release a mixed window, or create the agent).
    Every pass fits the one operation budget; a failure leaves at most one
    step to recover.
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
        launch = ((checkout, list(agent)) if checkout and agent
                  and os.environ.get("STORYHOOK_VERIFIER_AGENT") != "0" else None)
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
        for name in ("verification", "verifier"):
            windows = {row[0] for row in rows if row[1] == name}
            if len(windows) > 1 or any(row[1] == name and row[5] != owner for row in rows):
                raise RuntimeError(f"{name} ownership conflict in session {session}")
        if readopt_view(rows, owner):
            rows = inventory(session)
        own = [row for row in rows if row[1] == "verification"]
        agents = [row for row in rows if is_agent(row, owner)]
        if len(agents) > 1 or any(row[5] != owner for row in agents):
            raise RuntimeError(f"verifier agent ownership conflict in session {session}")
        agent_window = [row for row in rows if row[1] == "verifier"]
        if agent_window and not any(is_agent(row, owner) for row in agent_window):
            raise RuntimeError(f"verifier occupant conflict in session {session}")
        # Move the exact legacy pane without restarting it. Disabled agents stay
        # where they are; that switch never authorizes ending a live process.
        legacy = [row for row in agents if row[1] != "verifier"]
        if legacy and launch:
            if agent_window:
                raise RuntimeError(f"verifier ownership conflict in session {session}")
            migrate_agent(session, [row for row in rows if row[0] == legacy[0][0]], owner, legacy[0])
            return
        reader_row = next((row for row in own if is_reader(row)), None)
        if reader_row and healthy(reader_row):
            if reap_temporary(session, rows, owner):
                return
            if launch and not agents:
                ensure_agent(session, own, owner, launch)
            return
        # Even a dead user pane is a person's work. Only a single proven reader
        # may be retired; mixed/replaced panes are preserved and released.
        if own and (len(own) != 1 or not reader_like(own[0])):
            release_reader_window(session, own)
            return
        replace_window(session, own, owner, reader)


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


def release_reader_window(session, own):
    """Preserve a mixed or repurposed window; a later pass allocates its reader."""
    window = own[0][0]
    if [row for row in inventory(session) if row[0] == window] != own:
        raise RuntimeError("verification ownership changed before release")
    # A disabled legacy agent retains ownership until a later enabled pass
    # can migrate it; never forget it and launch a duplicate.
    ownership = ([] if any(is_agent(row, row[5]) for row in own) else
                 ["set-option", "-w", "-t", window, "@storyhook-view-released", own[0][5], ";",
                  "set-option", "-w", "-u", "-t", window, "@storyhook-journal", ";"])
    tmux("rename-window", "-t", window, "verification-retained-" + uuid.uuid4().hex, ";",
         *ownership,
         "set-option", "-w", "-u", "-t", window, "@storyhook-reader", ";",
         "set-option", "-w", "-u", "-t", window, "@storyhook-command")


def agent_clock(owner):
    """Session ownership outlives either window, including an operator close."""
    return "@storyhook-agent-started-" + owner


def migrate_agent(session, own, owner, agent):
    """Break out one positively identified legacy agent, preserving its PID."""
    if [row for row in inventory(session) if row[0] == agent[0]] != own:
        raise RuntimeError("verification ownership changed before agent migration")
    # Preserve the old cooldown even if the reader is later replaced or closed.
    started = agent[9] if agent[9].isdigit() else str(int(time.time()))
    session_id = tmux("display-message", "-p", "-t", "=" + session, "#{session_id}")
    target = "=" + session + ":=verifier"
    move = (["rename-window", "-t", agent[0], "verifier"] if len(own) == 1 else
            ["break-pane", "-d", "-s", agent[2], "-t", "=" + session + ":", "-n", "verifier"])
    release = ([";", "set-option", "-w", "-t", agent[0], "@storyhook-view-released", owner,
                ";", "set-option", "-w", "-u", "-t", agent[0], "@storyhook-journal"]
               if len(own) > 1 and agent[1] != "verification" else [])
    tmux("set-option", "-t", session_id, agent_clock(owner), started, ";",
         *move, ";",
         "set-option", "-w", "-t", target, "@storyhook-journal", owner, ";",
         "set-option", "-w", "-t", target, "automatic-rename", "off", ";",
         "set-option", "-w", "-t", target, "allow-rename", "off", *release)


def ensure_agent(session, own, owner, launch):
    """Create the missing agent in its own detached window, after cooldown."""
    session_id = tmux("display-message", "-p", "-t", "=" + session, "#{session_id}")
    started = tmux("show-option", "-q", "-v", "-t", session_id, agent_clock(owner))
    # Read the legacy clock until the first new launch/migration publishes it.
    if not started and own:
        started = own[0][9]
    if started.isdigit() and time.time() - int(started) < AGENT_RESPAWN_COOLDOWN:
        return
    checkout, command = launch
    scrub_owned_session(tmux, session, os.environ)
    target = "=" + session + ":=verifier"
    tmux("set-option", "-t", session_id, agent_clock(owner), str(int(time.time())), ";",
         "new-window", "-d", "-t", "=" + session + ":", "-n", "verifier", "-c", checkout,
         *pane_overrides(os.environ), "/bin/sh", "-c", AGENT_LOOP, AGENT_MARKER + owner, *command, ";",
         "set-option", "-w", "-t", target, "@storyhook-journal", owner, ";",
         "set-option", "-w", "-t", target, "automatic-rename", "off", ";",
         "set-option", "-w", "-t", target, "allow-rename", "off")


def rollback(window, original):
    """Use only remaining time, keeping the original failure if cleanup also fails."""
    try:
        tmux("kill-window", "-t", window)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as cleanup:
        raise RuntimeError(f"{original}; cleanup of owned window {window} failed: {cleanup}; "
                           "a later reconcile will recover it") from original


def reap_temporary(session, rows, owner):
    """Only retire interrupted allocations after a permanent view exists."""
    for row in rows:
        if (row[1].startswith(("verification-pending-", ".verification-")) and row[5] == owner
                and len([other for other in rows if other[0] == row[0]]) == 1):
            current = [other for other in inventory(session) if other[0] == row[0]]
            if current == [row]:
                tmux("kill-window", "-t", row[0])
                return True
    return False


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
