"""Runner adapters: disabled passthrough, an inherited grant, or a root admission (SH-869).

One mode is resolved before anything runs (docs/spec/verification-
throughput-and-recovery.md, "SH-869 runner adoption"):

- Disabled: no host policy exists. Exec the command unchanged.
- Inherit: this process descends from a granted root. Run inside that grant;
  a pool takes its unit count from the inherited share. No broker call.
- Root: enqueue, wait and supervise the command under `ManagedProcess`.

Every mode exports `STORYHOOK_HOST_ENTRY=<entry>:<pid>`, which lets a runner
that re-executes itself through this adapter recognise its own admission
(`$$` in place, `$PPID` under a supervisor) without trusting an ancestor's.
"""

import argparse
import json
import math
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time
import fcntl

from .activation import load_policy
from .entries import ENTRIES
from .namespace import ROOT
from .native import host_identity
from .policy import Refusal
from .reservation import Admission, Reservation, amount, drain_cause

GRANT, REQUEST, LEASE_FD = "STORYHOOK_HOST_GRANT", "STORYHOOK_HOST_REQUEST", "STORYHOOK_HOST_LEASE_FD"
SHARE, UNITS, ENTRY = "STORYHOOK_HOST_SHARE", "STORYHOOK_HOST_UNITS", "STORYHOOK_HOST_ENTRY"

# How often a wait is reported again, on stderr and to a gate journal. Equal
# to rustc-slot.py's WAIT_REPORT_SECS: the gate watchdog reads journal growth,
# so a wait for admission is reported as the build-slot wait is.
WAIT_REPORT_SECS = 36

# Exit status of an admission refusal or withdrawal, as `host-admission.py run`.
ADMISSION_STATUS = 125


def disabled(root=ROOT):
    """True only when no host policy exists; any other fault fails loudly."""
    try:
        os.lstat(Path(root) / "policy.json")
    except FileNotFoundError:
        return True
    return False


def parse_share(text):
    """`cpu=<milli>,memory=<bytes>`; a malformed share is never read as zero."""
    try:
        fields = dict(part.split("=", 1) for part in text.split(","))
        value = {key: int(fields[key]) for key in ("cpu", "memory")}
    except (KeyError, ValueError) as error:
        raise Refusal(f"malformed inherited share {text!r}") from error
    if set(fields) != {"cpu", "memory"} or min(value.values()) < 0:
        raise Refusal(f"malformed inherited share {text!r}")
    return value


def format_share(value):
    """The inverse of `parse_share`."""
    return f"cpu={value['cpu']},memory={value['memory']}"


def held_lease_descriptor(root, env):
    """A descriptor this process inherited for a live lease guard under `root`.

    Proof is a fresh open of the same inode plus a non-blocking lock probe
    that fails because the supervisor holds it. The inherited descriptor is
    never locked itself: that would take an unheld guard.
    """
    root = Path(root)
    candidates = []
    if env.get(LEASE_FD, "").isdecimal():
        candidates.append(int(env[LEASE_FD]))
    else:
        candidates.extend(int(name) for name in os.listdir("/dev/fd") if int(name) > 2)
    for fd in candidates:
        try:
            info = os.fstat(fd)
        except OSError:
            continue
        if not stat.S_ISREG(info.st_mode):
            continue
        try:
            names = [p for p in root.iterdir() if p.name.startswith("lease-") and p.name.endswith(".lock")]
        except OSError:
            return None
        for path in names:
            try:
                same = path.lstat()
            except OSError:
                continue
            if (same.st_dev, same.st_ino) != (info.st_dev, info.st_ino):
                continue
            probe = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
            try:
                fcntl.flock(probe, fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError:
                return fd
            finally:
                os.close(probe)
    return None


def inherited(root, env):
    """Whether this process runs inside a granted root of the authority at `root`."""
    if env.get(GRANT) and env.get(REQUEST):
        return True
    if env.get(GRANT) or env.get(REQUEST):
        raise Refusal("incomplete inherited grant: both STORYHOOK_HOST_GRANT and _REQUEST are required")
    return held_lease_descriptor(root, env) is not None


def nested_units(entry, requested, env, policy):
    """(units, per-worker share) for a pool inside an inherited grant.

    The inherited share bounds every level: `units * unit + overhead` never
    exceeds it, so nesting cannot multiply a grant. A share smaller than one
    unit still runs one worker, the share's sole occupant. Without a share
    (a launcher that dropped the environment but kept the descriptor) the
    pool runs serially.
    """
    if not env.get(SHARE):
        return 1, None
    share = parse_share(env[SHARE])
    unit = amount(policy, entry.unit)
    overhead = amount(policy, entry.overhead) if entry.overhead else dict(cpu=0, memory=0)
    room = {k: max(0, share[k] - overhead[k]) for k in share}
    fit = min(room[k] // unit[k] for k in unit)
    units = max(1, min(requested, fit))
    return units, {k: room[k] // units for k in room}


def project_label():
    """The canonical Git common directory, the scripts' project key; else the cwd."""
    try:
        common = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
                                capture_output=True, text=True, check=True).stdout.strip()
        return os.path.realpath(common) if common else os.getcwd()
    except (OSError, subprocess.CalledProcessError):
        return os.getcwd()


class Reporter:
    """Admission waits and outcomes, on stderr and in a watching gate journal."""

    def __init__(self, entry, request_id, env=os.environ):
        self.entry, self.journal = entry, env.get("STORYHOOK_GATE_PROGRESS")
        base = env.get("STORYHOOK_GATE_PROGRESS_PATH") or "release gate"
        # One activity per request: concurrent waits of one entry stay distinct.
        self.path = f"{base}/host admission/{request_id}"
        self.label = f"waiting for host admission ({entry})"
        self.cost_id, self.started, self.reported = None, None, None

    def write(self, *records):
        if not self.journal:
            return
        try:
            with open(self.journal, "a", encoding="utf-8") as stream:
                for record in records:
                    stream.write(json.dumps(dict(record, at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))) + "\n")
        except OSError as error:
            print(f"host-admit: could not append to the gate progress journal {self.journal}: {error}",
                  file=sys.stderr)

    def waiting(self, row):
        """Report a queued turn now and then every WAIT_REPORT_SECS."""
        now = time.monotonic()
        if self.started is None:
            self.started = now
            self.cost_id = f"host-admission-{os.getpid()}-{time.monotonic_ns()}"
            self.write(dict(kind="activity", path=self.path, label=self.label, status="running"),
                       dict(kind="cost", event="start", phase="resource-wait", id=self.cost_id,
                            path=self.path, monotonic_ns=time.monotonic_ns()))
        elif now - self.reported < WAIT_REPORT_SECS:
            return
        else:
            self.write(dict(kind="activity", path=self.path, label=self.label, status="running"))
        self.reported = now
        position = row.get("queue_position") or "?"
        print(f"host-admit: {self.entry} is waiting for host admission "
              f"({int(now - self.started)}s; {row.get('reason') or 'queued'}; position {position})",
              file=sys.stderr, flush=True)

    def granted(self):
        if self.started is None:
            return
        self.write(dict(kind="activity", path=self.path, label=f"host admission granted ({self.entry})",
                        status="passed"),
                   dict(kind="cost", event="end", phase="resource-wait", id=self.cost_id,
                        path=self.path, monotonic_ns=time.monotonic_ns()))
        print(f"host-admit: {self.entry} admitted after {time.monotonic() - self.started:.1f}s",
              file=sys.stderr, flush=True)

    def failed(self, admission):
        """The cause of a refusal or withdrawal; a process fault, never a test verdict."""
        self.write(dict(kind="admission", entry=self.entry, cause=admission.cause,
                        retryable=admission.retryable, reason=admission.reason))
        print(f"host-admit: {self.entry}: {admission}", file=sys.stderr, flush=True)


def child_env(env, entry_id, pid, extra):
    """The command's environment: the caller's, this admission's marker and grant."""
    result = dict(env)
    result[ENTRY] = f"{entry_id}:{pid}"
    result.update(extra)
    return result


def admit(entry_id, requested, command, *, root=ROOT, policy_loader=load_policy,
          env=None, execute=os.execvpe, project=None):
    """Run `command` as `entry_id`; returns an exit status unless `execute` replaces this process."""
    env = dict(os.environ if env is None else env)
    entry = ENTRIES[entry_id]
    if not command:
        raise Refusal("host-admit requires a command after --")
    if requested < 1:
        raise Refusal("units must be positive")
    pid = os.getpid()
    if disabled(root):
        extra = {UNITS: str(requested)} if entry.pool else {}
        return execute(command[0], command, child_env(env, entry_id, pid, extra))
    if inherited(root, env):
        if not entry.pool:
            return execute(command[0], command, child_env(env, entry_id, pid, {}))
        policy = policy_loader(root, host_identity())
        units, share = nested_units(entry, requested, env, policy)
        extra = {UNITS: str(units)}
        if share is not None:
            extra[SHARE] = format_share(share)
        return execute(command[0], command, child_env(env, entry_id, pid, extra))
    return supervise(entry, requested, command, root=root, policy_loader=policy_loader,
                     env=env, project=project)


def supervise(entry, requested, command, *, root, policy_loader, env, project):
    """Root mode: wait for the grant, then run the command in its own managed session."""
    from .supervisor import ManagedProcess

    reporter = None
    interrupted = None
    prior = {}

    def interrupt(signum, _frame):
        nonlocal interrupted
        interrupted = signum

    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        prior[signum] = signal.signal(signum, interrupt)
    try:
        try:
            reservation = Reservation(entry.id, project=project or project_label(), requested=requested,
                                      root=root, policy_loader=policy_loader)
            reporter = Reporter(entry.id, reservation.id, env)
            lease = reservation.acquire(reporter.waiting, lambda: interrupted is not None)
        except Admission as admission:
            (reporter or Reporter(entry.id, "refused", env)).failed(admission)
            return ADMISSION_STATUS
        if lease is None:
            return 128 + interrupted
        reporter.granted()
        try:
            managed = ManagedProcess(reservation.client, lease, command,
                                     env=child_env(env, entry.id, os.getpid(), reservation.child_env()))
        except (Refusal, OSError) as error:
            # The constructor drains a started child itself; an unlaunched
            # grant is released here.
            reservation.abandon()
            reporter.failed(Admission("launch-failed", False, str(error)))
            return ADMISSION_STATUS
        try:
            result = managed.wait(force_cancel=interrupted is not None)
        finally:
            managed.close()
        if managed.drain_reason and managed.drain_reason != "client cancellation":
            cause, retryable = drain_cause(managed.drain_reason)
            reporter.failed(Admission(cause, retryable, managed.drain_reason))
            return ADMISSION_STATUS
        if interrupted is not None:
            return 128 + interrupted
        return result if result >= 0 else 128 - result
    finally:
        for signum, handler in prior.items():
            signal.signal(signum, handler)


def drain_seconds(root=ROOT, policy_loader=load_policy):
    """Whole seconds an enclosing lock must allow a root to drain; 0 when disabled."""
    if disabled(root):
        return 0
    timing = policy_loader(root, host_identity()).value
    return math.ceil((2 * timing["cleanup_ms"] + timing["sample_ms"]) / 1000)


def main(arguments=None, *, root=ROOT, policy_loader=load_policy):
    """The `host-admit.py` command line; fixtures inject `root` in-process only."""
    parser = argparse.ArgumentParser(prog="host-admit.py", description=__doc__.splitlines()[0])
    parser.add_argument("--entry", choices=sorted(ENTRIES))
    parser.add_argument("--units", type=int, default=1)
    parser.add_argument("--drain-seconds", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(arguments)
    try:
        if args.drain_seconds:
            print(drain_seconds(root, policy_loader))
            return 0
        if not args.entry:
            parser.error("--entry is required")
        command = args.command[1:] if args.command[:1] == ["--"] else args.command
        return admit(args.entry, args.units, command, root=root, policy_loader=policy_loader)
    except (Refusal, OSError, ValueError) as error:
        print(f"host-admit: {error}", file=sys.stderr)
        return ADMISSION_STATUS
