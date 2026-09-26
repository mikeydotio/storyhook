#!/usr/bin/env python3
"""Serialize verifier mutations and supervise gate sessions (SH-683).

Lock order is project gate, workspace flock, merge. The inode is permanent.
A launch handshake makes a journaled session identity precede execution.
Unknown same-boot arbitrary execution is retained, never guessed quiescent.
Every gate session starts at the verifier's scheduling class (SH-785).
"""

import fcntl
import json
import os
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

sys.dont_write_bytecode = True
from verifier_state import Refusal, boot, held, paths, read, save, session_members
from verifier_result import EXECUTION_FILE, attach_cleanup, cleanup_failure, publish_execution


class CleanupRefusal(Refusal):
    """An observed command exit whose subsequent supervision could not finish."""


# The scheduling class of every gate session, per platform, as (tool, arguments)
# pairs that each exec the next word (SH-785). A gate runs for minutes beside
# agent sessions, hooks and the daemon; at their class it starves them and
# hooks time out. taskpolicy(8) is Apple's supported way to spawn under a QoS
# clamp, and utility also lowers the I/O tier. `-b` or `-c background` would
# confine the gate to the efficiency cores. Linux has no clamp: nice lowers the
# CPU share relative to the supervisor, ionice the disk share.
GATE_CLASS = {
    "darwin": [("/usr/sbin/taskpolicy", ["-c", "utility"])],
    "linux": [("nice", ["-n", "10"]), ("ionice", ["-c2", "-n7"])],
}

# The gate's launch report (SH-702). The launcher writes LAUNCHING immediately
# before its exec, and LAUNCH_FAILED if the exec does not replace it. Only a
# report of exactly LAUNCHING proves the gate command itself began: a class
# tool that dies before the launcher leaves the report empty.
LAUNCHING = b"exec"
LAUNCH_FAILED = b"failed"

# The launcher is this file, run again behind the class tools.
SELF = os.path.abspath(__file__)


def gate_class(platform=None, path=None, classes=GATE_CLASS):
    """Return the absolute argv prefix that starts a gate at the verifier's class.

    Each tool is resolved before any gate starts, so a missing one is refused
    by name rather than failing between the supervisor and the gate, where
    its exit status would read as the gate's. A platform with no chosen class
    is refused too: a gate never silently runs at the class of its caller.
    """
    platform = sys.platform if platform is None else platform
    tools = classes.get(platform)
    if tools is None:
        raise Refusal(f"no gate scheduling class is defined for platform {platform!r}; "
                      "the verifier runs a gate only at a class chosen for its platform (SH-785)")
    search = os.environ.get("PATH", os.defpath) if path is None else path
    prefix = []
    for tool, arguments in tools:
        found = tool if os.path.isabs(tool) else shutil.which(tool, path=search)
        if found is None:
            raise Refusal(f"cannot start gates at their scheduling class: {tool} is not on PATH ({search})")
        # The supervisor runs in the candidate worktree; a relative PATH entry
        # would let the candidate's own files stand in for a class tool.
        if not os.path.isabs(found):
            raise Refusal(f"cannot start gates at their scheduling class: {tool} resolved to {found}, "
                          "which is not an absolute path")
        if not (os.path.isfile(found) and os.access(found, os.X_OK)):
            raise Refusal(f"cannot start gates at their scheduling class: {found} is not an executable file")
        prefix += [found, *arguments]
    return prefix


def launch(report, command):
    """Become the gate command behind the class tools; never returns.

    Signals keep their default dispositions here (main dispatches this before
    installing any handler), so a cancellation that arrives now ends the
    launch instead of being latched while the gate starts anyway.
    """
    try:
        os.set_inheritable(report, False)
        os.write(report, LAUNCHING)
        os.execvpe(command[0], command, os.environ)
    except BaseException as error:
        # Any failure to exec, including an interrupt, is a launch failure:
        # reported to the supervisor and never mistaken for a gate's answer.
        try:
            os.write(report, LAUNCH_FAILED)
            print(f"verifier-owner: could not launch {command[0] if command else 'an empty command'}: "
                  f"{error!r}", file=sys.stderr)
        finally:
            os._exit(125)


def read_report(fd):
    """Read the whole launch report without waiting on other holders of the pipe.

    Every write precedes the leader's exit, so once that exit is observed the
    pipe already holds the report. Waiting for end-of-file instead would let
    any process that kept the write end (a PATH shim that forked) stall the
    supervisor before its reaping ladder.
    """
    os.set_blocking(fd, False)
    report = b""
    while True:
        try:
            chunk = os.read(fd, 64)
        except BlockingIOError:
            return report
        if not chunk:
            return report
        report += chunk


class Cancellation:
    """Latch signals before admitting children; handlers perform no I/O."""

    def __init__(self):
        self.signum = None
        for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
            signal.signal(signum, self.request)

    def request(self, signum, _frame):
        """The first request is irreversible; repeated signals cannot reenter I/O."""
        if self.signum is None:
            self.signum = signum


def signal_session(sid, signum):
    """Signal only currently confirmed members of an owned session."""
    for pid in session_members(sid):
        try:
            os.kill(pid, signum)
        except ProcessLookupError:
            pass


def cleanup_budget():
    """Read the cleanup policy once, before any lifecycle record is touched."""
    budget_ms = os.environ.get("STORYHOOK_VERIFIER_CLEANUP_GRACE_MS", "30000")
    if not budget_ms.isascii() or not budget_ms.isdecimal() or len(budget_ms) > 8 or int(budget_ms) < 4000:
        raise Refusal("verifier cleanup budget must be 4000..99999999 milliseconds")
    return int(budget_ms) / 1000


def note(record_path, **fields):
    """Publish a durable change to the current record, keeping its other fields."""
    record = read(record_path)
    record.update(fields)
    save(record_path, record)


# Observing an exit without reaping keeps the leader a zombie, so its pid and
# session identity cannot be reused while survivors are still being signalled.
# The portable fallback reaps at observation and accepts that window (SH-695).
PINNED = hasattr(os, "waitid")


def observe_exit(child):
    """Report the leader's translated exit code once it has exited, else None."""
    if PINNED:
        info = os.waitid(os.P_PID, child, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        if info is None:
            return None
        return info.si_status if info.si_code == os.CLD_EXITED else 128 + info.si_status
    waited, status = os.waitpid(child, os.WNOHANG)
    if not waited:
        return None
    code = os.waitstatus_to_exitcode(status)
    return code if code >= 0 else 128 - code


def supervisor_gone(record_path, session):
    """A recorded gate supervisor is gone when no such process is in the session."""
    supervisor = read(record_path).get("gate_supervisor")
    if supervisor is None:
        return True
    try:
        return os.getsid(supervisor) != session
    except ProcessLookupError:
        return True


def execute(command, record_path, record, field, cancellation, budget, output=None, gate_prefix=None):
    """Admit a new session only after its identity is durably recorded.

    With gate_prefix, the leader execs the class tools, which exec the launcher
    in this file, which execs the command: one pid throughout, so the recorded
    session is the gate's, and the launch report still speaks for the command.
    """
    receive, release = os.pipe()
    exec_error, exec_report = os.pipe()
    execution_path = os.environ.get(EXECUTION_FILE) if field == "gate_session" else None
    child = os.fork()
    if child == 0:
        os.close(release)
        os.close(exec_error)
        try:
            os.setsid()
            if os.read(receive, 1) != b"1":
                os._exit(125)
            os.close(receive)
            for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
                signal.signal(signum, signal.SIG_DFL)
            if output is not None:
                os.dup2(output.fileno(), 1)
            environment = dict(os.environ)
            if field == "gate_session":
                environment.pop(EXECUTION_FILE, None)
            if gate_prefix is not None:
                # Open across the class tools' execs; the launcher closes it at
                # the gate's own exec, so the gate never holds the report.
                os.set_inheritable(exec_report, True)
                command = [*gate_prefix, sys.executable, SELF, "launch", str(exec_report), "--", *command]
            os.execvpe(command[0], command, environment)
        except (OSError, ValueError) as error:
            os.write(exec_report, LAUNCH_FAILED)
            print(f"verifier-owner: could not launch {command[0]}: {error}", file=sys.stderr)
            os._exit(125)
    os.close(receive)
    os.close(exec_report)
    record[field] = child
    save(record_path, record)
    # Cancellation before admission closes the handshake without executing.
    admitted = cancellation.signum is None
    if admitted:
        os.write(release, b"1")
    os.close(release)
    grace = budget / (4 if field == "gate_session" else 2)
    deadline = None
    killed_at = None
    code = None
    try:
        while True:
            # Everything this pass observes is at least as recent as this.
            observed_at = time.monotonic()
            if cancellation.signum is not None and deadline is None:
                deadline = time.monotonic() + grace
                if field == "gate_session":
                    signal_session(child, cancellation.signum)
                else:
                    # The lifecycle leader may be waiting on a nested shell.
                    # Broadcasting here kills its introspection workers before the
                    # inner supervisor can durably establish gate quiescence.
                    supervisor = read(record_path).get("gate_supervisor")
                    if supervisor is not None:
                        try:
                            if os.getsid(supervisor) == child:
                                os.kill(supervisor, cancellation.signum)
                        except ProcessLookupError:
                            pass
                    try:
                        os.kill(child, cancellation.signum)
                    except ProcessLookupError:
                        pass
            if code is None:
                code = observe_exit(child)
                if code is not None and execution_path:
                    report = read_report(exec_error)
                    launched = report == LAUNCHING if gate_prefix is not None else not report
                    publish_execution(execution_path, code, admitted and launched)
                if code is not None and field == "gate_session":
                    # Recorded before any census: a later owner can then tell an
                    # exited leader from an interrupted gate of unknown state.
                    note(record_path, gate_leader_exit=code)
            if code is None and deadline is None:
                # Healthy execution needs no machine-wide process census.
                time.sleep(.1)
                continue
            sessions = {child}
            if field == "session":
                gate = read(record_path).get("gate_session")
                if gate:
                    sessions.add(gate)
            remaining = [pid for sid in sessions for pid in session_members(sid)]
            if code is not None and not remaining:
                break
            if code is not None and deadline is None:
                # The leader has answered; what survives it is a leak, not a
                # writer with authority. Reap it on the cancellation ladder rather
                # than failing the whole queue closed over it (SH-695).
                print(f"verifier-owner: {field} {child} exited {code} leaving survivors {remaining};"
                      " signalling TERM, reaping within the cleanup grace", file=sys.stderr)
                deadline = time.monotonic() + grace
                signal_session(child, signal.SIGTERM)
                if field == "session":
                    gate = read(record_path).get("gate_session")
                    if gate and supervisor_gone(record_path, child):
                        signal_session(gate, signal.SIGTERM)
            if deadline is not None and time.monotonic() >= deadline and killed_at is None:
                signal_session(child, signal.SIGKILL)
                # Escalation must also cover the recorded arbitrary execution
                # session if its supervisor died before completing the record.
                if field == "session":
                    gate = read(record_path).get("gate_session")
                    if gate:
                        signal_session(gate, signal.SIGKILL)
                killed_at = time.monotonic()
            # The reaping eighth runs from the delivered SIGKILL, and only members
            # found by a census begun after it closed prove survival. Under load
            # one census can outlast the eighth, and a census taken before the
            # kill, or empty because the leader has just exited, proves nothing
            # about the kill (SH-767).
            if killed_at is not None and remaining and observed_at >= killed_at + budget / 8:
                raise Refusal(f"could not reap execution session {child} after SIGKILL; live writers={remaining}; retained {record_path}")
            time.sleep(.05)
        if PINNED:
            os.waitpid(child, 0)
        return code
    except (Refusal, OSError, ValueError, subprocess.SubprocessError) as error:
        if code is not None:
            raise CleanupRefusal(f"{field} exited {code}; {error}; retained {record_path}") from error
        raise
    finally:
        os.close(exec_error)


def run(mode, common, worktree, key, command, cancellation, output=None):
    """Hold kernel exclusion and refuse uncertain previous execution."""
    owner_path = str(key) + ".owner"
    if mode == "held":
        return 0 if held(common, worktree, key) else 1
    if mode == "gate":
        if not held(common, worktree, key):
            raise Refusal("gate execution has no matching verifier owner")
        # Policy is validated before the record says a gate started, so a
        # refused budget or class cannot leave an interrupted gate behind (SH-695).
        budget = cleanup_budget()
        prefix = gate_class()
        owner = read(owner_path)
        owner["gate_started"] = True
        owner["gate_supervisor"] = os.getpid()
        owner["gate_leader_exit"] = None
        save(owner_path, owner)
        status = execute(command, owner_path, owner, "gate_session", cancellation, budget, gate_prefix=prefix)
        owner = read(owner_path)
        owner["gate_started"] = False
        owner["gate_session"] = None
        owner["gate_supervisor"] = None
        owner["gate_leader_exit"] = None
        save(owner_path, owner)
        return status
    if mode != "run" or not command:
        raise Refusal("usage: verifier-owner.py held|run|gate <common> <worktree> [-- command...]")
    budget = cleanup_budget()
    lock = str(key) + ".lock"
    fd = os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    os.set_inheritable(fd, True)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise Refusal(f"live verifier owner holds {lock}; inspect {owner_path}; worktree={worktree}") from error
        previous = read(owner_path)
        if previous:
            if previous.get("common") != str(common) or previous.get("worktree") != str(worktree):
                raise Refusal(f"ownership mapping conflict in {owner_path}")
            try:
                uuid.UUID(previous["boot"])
                uuid.UUID(previous["nonce"])
                if type(previous["supervisor"]) is not int or previous["supervisor"] <= 0:
                    raise ValueError("supervisor is not a positive PID")
                session = previous["session"]
                if session is None:
                    if previous.get("completed") is not True:
                        raise ValueError("missing session without durable completion")
                elif type(session) is not int or session <= 0:
                    raise ValueError("session is not a positive identity")
                if type(previous["gate_started"]) is not bool:
                    raise ValueError("gate_started is not a boolean")
                # Absent in records written before SH-695: read as unknown.
                leader_exit = previous.get("gate_leader_exit")
                if leader_exit is not None:
                    if type(leader_exit) is not int or leader_exit < 0:
                        raise ValueError("gate_leader_exit is not a non-negative exit code")
                    if not previous["gate_started"] or type(previous.get("gate_session")) is not int:
                        raise ValueError("gate leader exit recorded without its started gate session")
            except (KeyError, ValueError, TypeError) as error:
                raise Refusal(f"incomplete owner identity in {owner_path}: {error}") from error
            if previous.get("boot") == boot():
                for field in ("session", "gate_session"):
                    if previous.get(field):
                        members = session_members(previous[field])
                        if members:
                            raise Refusal(f"previous live owner session {previous[field]} has writers {members}; {owner_path}")
                # A recorded leader exit plus the empty census above is the
                # evidence that the supervised gate session finished; without
                # the exit the gate was interrupted and its state is unknown.
                if previous.get("gate_started") and previous.get("gate_leader_exit") is None:
                    raise Refusal(f"interrupted arbitrary gate has ambiguous ownership on this boot; preserve {owner_path} and {worktree}; establish writer quiescence before recovery")
        owner = {"version": 1, "common": str(common), "worktree": str(worktree),
                 "nonce": uuid.uuid4().hex, "boot": boot(), "gate_started": False,
                 "supervisor": os.getpid()}
        os.environ["STORYHOOK_VERIFIER_OWNER"] = owner["nonce"]
        status = execute(command, owner_path, owner, "session", cancellation, budget, output)
        try:
            owner = read(owner_path)
            owner["session"] = None
            owner["completed"] = True
            save(owner_path, owner)
        except (Refusal, OSError, ValueError) as error:
            raise CleanupRefusal(f"lifecycle exited {status}; final owner update failed: {error}") from error
        return status
    finally:
        # Closing our descriptor does not release another participant's copy.
        os.close(fd)


def main():
    """Expose internal owner operations with contextual, nonzero refusals."""
    if sys.argv[1:2] == ["launch"]:
        # Before Cancellation(): a latched TERM would let the gate start anyway.
        command = sys.argv[3:]
        if command[:1] == ["--"]:
            command = command[1:]
        launch(int(sys.argv[2]), command)
    cancellation = Cancellation()
    as_json = sys.argv[1:2] == ["run-json"]
    try:
        mode, common_arg, worktree_arg = sys.argv[1:4]
        if as_json:
            mode = "run"
        common, worktree, key = paths(common_arg, worktree_arg)
        command = sys.argv[4:]
        if command[:1] == ["--"]:
            command = command[1:]
        if as_json:
            # Publish a child verdict only after its process tree is settled.
            # A file cannot deadlock when a surviving child retains stdout.
            with tempfile.TemporaryFile() as output:
                try:
                    status = run(mode, common, worktree, key, command, cancellation, output)
                except CleanupRefusal as error:
                    output.seek(0)
                    try:
                        value = json.load(output)
                        failure = cleanup_failure(common, worktree, "owner cleanup", str(error))
                        value = attach_cleanup(value, failure)
                    except (ValueError, Refusal) as invalid:
                        raise Refusal(f"{error}; child verdict unavailable: {invalid}") from error
                    print(json.dumps(value))
                    return 0
                output.seek(0)
                shutil.copyfileobj(output, sys.stdout.buffer)
                return status
        return run(mode, common, worktree, key, command, cancellation)
    except (Refusal, OSError, ValueError, subprocess.SubprocessError) as error:
        detail = f"verifier-owner: {error}"
        print(detail, file=sys.stderr)
        if as_json:
            print(json.dumps({"result": "infrastructure-failure", "disposition": "permanent", "detail": detail}))
            return 0
        return 1


if __name__ == "__main__":
    sys.exit(main())
