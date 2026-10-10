"""Narrow descriptive-exposure timeout policy; never admission/custody authority."""

import fcntl
import hashlib
import json
import os
import re
from pathlib import Path
import time

from host_admission import native
from host_admission.namespace import open_private
from host_admission.policy import Refusal as CustodyRefusal
from verifier_state import Refusal

COMMANDS = {
    "processes": ["ps", "-axo", "pid=,ppid=,pcpu=,comm="],
    "resource_processes": ["ps", "-axo", "pid=,ppid=,pcpu=,rss=,comm="],
}
CONTRACT = {"version": 1, "optional_helpers": COMMANDS, "local_seconds": 30,
            "timeout_effect": "incomplete-stop-sampling", "quiescence": "native-session-and-guard"}


class LocalDeadline(CustodyRefusal):
    """Internal cause retained by the supervisor, never matched by error text."""


class ExposureTimeout(Refusal):
    def __init__(self, proof):
        super().__init__("descriptive helper local deadline; native quiescence proved")
        self.proof = proof


def require_optional(argv, seconds, overall_end):
    if (argv not in COMMANDS.values() or seconds != 30
            or type(overall_end) not in (int, float)
            or not time.monotonic() < overall_end < float("inf")):
        raise Refusal("optional observation lacks a distinct local allowance")


def quiescence(directory, *, expected=None):
    """Observe failed ownership without settling, finishing or rewriting it."""
    from build_products import read_record
    directory = Path(directory)
    if directory.resolve() != directory or directory.is_symlink():
        raise Refusal("helper custody namespace changed")
    path = directory / "record.json"
    before = path.read_bytes()
    row = read_record(path)
    if (json.loads(before) != row or not re.fullmatch(r"[a-f0-9]{32}", row.get("token", ""))
            or row.get("id") != row["token"]):
        raise Refusal("helper custody record identity changed")
    executions = row.get("executions")
    if (row.get("state") != "running" or not isinstance(executions, list)
            or len(executions) != 1 or row.get("command") not in COMMANDS.values()):
        raise Refusal("helper failure has no exact retained execution")
    execution = executions[0]
    sid = execution.get("session")
    if (type(sid) is not int or sid <= 0 or execution.get("leader", {}).get("pid") != sid
            or execution.get("leader", {}).get("boot", "").lower() != native.boot_identity().lower()
            or execution.get("guard") != "lease-" + row["token"] + ".lock"):
        raise Refusal("helper execution identity changed")
    # Native absence and the independent lifetime lock are both necessary.
    # No descriptive ps result, age or finished flag can substitute for either.
    if native.session_members(sid):
        raise Refusal("timed-out helper has surviving native participants")
    fd = open_private(directory / execution["guard"])
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        facts = os.fstat(fd)
        named = (directory / execution["guard"]).lstat()
        if (facts.st_dev, facts.st_ino) != (named.st_dev, named.st_ino):
            raise Refusal("helper lifetime guard was substituted")
        if native.session_members(sid):
            raise Refusal("helper session changed during quiescence proof")
        if path.read_bytes() != before:
            raise Refusal("helper record changed during quiescence proof")
        proof = dict(directory=str(directory), record_sha256=hashlib.sha256(before).hexdigest(),
                     execution=execution, guard_device=facts.st_dev, guard_inode=facts.st_ino)
        if expected is not None and proof != expected:
            raise Refusal("helper quiescence proof changed")
        return proof
    except BlockingIOError as error:
        raise Refusal("timed-out helper lifetime guard remains held") from error
    finally:
        os.close(fd)


def completeness(value):
    """No missing/late observation can be replayed as complete under this policy."""
    if (not isinstance(value, dict) or set(value) != {"version", "complete", "gaps"}
            or type(value["version"]) is not int or value["version"] != 1
            or type(value["complete"]) is not bool or not isinstance(value["gaps"], list)
            or len(value["gaps"]) > 1):
        raise Refusal("missing or malformed telemetry completeness")
    for gap in value["gaps"]:
        if (not isinstance(gap, dict) or set(gap) != {"helper", "reason"}
                or gap["helper"] not in COMMANDS or gap["reason"] != "local-timeout-quiescent"):
            raise Refusal("unknown descriptive telemetry gap")
    if value["complete"] != (not value["gaps"]):
        raise Refusal("telemetry completeness contradicts retained gaps")
    return value["complete"]


def complete():
    return {"version": 1, "complete": True, "gaps": []}
