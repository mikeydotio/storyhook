#!/usr/bin/env python3
"""Serialize verifier mutations and supervise gate sessions (SH-683).

Lock order is project gate, workspace flock, merge. The inode is permanent.
A launch handshake makes a journaled session identity precede execution.
Unknown same-boot arbitrary execution is retained, never guessed quiescent.
"""

import fcntl
import json
import os
import signal
import shutil
import subprocess
import sys
import tempfile
import uuid

sys.dont_write_bytecode = True
from verifier_state import Refusal, boot, held, paths, read, save, session_members


def execute(command, record_path, record, field, output=None):
    """Admit a new session only after its identity is durably recorded."""
    receive, release = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(release)
        try:
            os.setsid()
            if os.read(receive, 1) != b"1":
                os._exit(125)
            os.close(receive)
            if output is not None:
                os.dup2(output.fileno(), 1)
            os.execvpe(command[0], command, os.environ)
        except (OSError, ValueError) as error:
            print(f"verifier-owner: could not launch {command[0]}: {error}", file=sys.stderr)
            os._exit(125)
    os.close(receive)
    record[field] = child
    save(record_path, record)
    os.write(release, b"1")
    os.close(release)

    def forward(signum, _frame):
        """Let the lifecycle leader order cancellation and its own cleanup."""
        # Broadcasting into the controlled lifecycle kills the gate supervisor's
        # introspection workers before it can durably establish quiescence.
        # Arbitrary gate sessions still require cancellation across subgroups.
        targets = session_members(child) if field == "gate_session" else [child]
        for pid in targets:
            try:
                os.kill(pid, signum)
            except ProcessLookupError:
                pass

    for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, forward)
    _, status = os.waitpid(child, 0)
    remaining = session_members(child)
    if remaining:
        raise Refusal(f"execution session {child} still has live writers {remaining}; retained {record_path}")
    code = os.waitstatus_to_exitcode(status)
    return code if code >= 0 else 128 - code


def run(mode, common, worktree, key, command, output=None):
    """Hold kernel exclusion and refuse uncertain previous execution."""
    owner_path = str(key) + ".owner"
    if mode == "held":
        return 0 if held(common, worktree, key) else 1
    if mode == "gate":
        if not held(common, worktree, key):
            raise Refusal("gate execution has no matching verifier owner")
        owner = read(owner_path)
        owner["gate_started"] = True
        save(owner_path, owner)
        status = execute(command, owner_path, owner, "gate_session")
        owner = read(owner_path)
        owner["gate_started"] = False
        owner["gate_session"] = None
        save(owner_path, owner)
        return status
    if mode != "run" or not command:
        raise Refusal("usage: verifier-owner.py held|run|gate <common> <worktree> [-- command...]")
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
            except (KeyError, ValueError, TypeError) as error:
                raise Refusal(f"incomplete owner identity in {owner_path}: {error}") from error
            if previous.get("boot") == boot():
                for field in ("session", "gate_session"):
                    if previous.get(field):
                        members = session_members(previous[field])
                        if members:
                            raise Refusal(f"previous live owner session {previous[field]} has writers {members}; {owner_path}")
                if previous.get("gate_started"):
                    raise Refusal(f"interrupted arbitrary gate has ambiguous ownership on this boot; preserve {owner_path} and {worktree}; establish writer quiescence before recovery")
        owner = {"version": 1, "common": str(common), "worktree": str(worktree),
                 "nonce": uuid.uuid4().hex, "boot": boot(), "gate_started": False,
                 "supervisor": os.getpid()}
        os.environ["STORYHOOK_VERIFIER_OWNER"] = owner["nonce"]
        status = execute(command, owner_path, owner, "session", output)
        owner = read(owner_path)
        owner["session"] = None
        owner["completed"] = True
        save(owner_path, owner)
        return status
    finally:
        # Closing our descriptor does not release another participant's copy.
        os.close(fd)


def main():
    """Expose internal owner operations with contextual, nonzero refusals."""
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
                status = run(mode, common, worktree, key, command, output)
                output.seek(0)
                shutil.copyfileobj(output, sys.stdout.buffer)
                return status
        return run(mode, common, worktree, key, command)
    except (Refusal, OSError, ValueError, subprocess.SubprocessError) as error:
        detail = f"verifier-owner: {error}"
        print(detail, file=sys.stderr)
        if as_json:
            print(json.dumps({"result": "infrastructure-failure", "disposition": "permanent", "detail": detail}))
            return 0
        return 1


if __name__ == "__main__":
    sys.exit(main())
