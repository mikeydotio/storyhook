"""Attempt-bound execution evidence, independent of verifier cleanup (SH-702)."""

import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
from verifier_state import Refusal, paths, read, save

EXECUTION_FILE = "STORYHOOK_GATE_EXECUTION_FILE"


def execution(path):
    """Read only evidence belonging to this exact lifecycle and attempt file."""
    value = read(path)
    if (not value or value.get("owner") != os.environ.get("STORYHOOK_VERIFIER_OWNER")
            or not value.get("owner") or value.get("attempt") != Path(path).name):
        raise Refusal(f"execution evidence has mismatched attempt identity: {path}")
    return value


def publish_execution(path, status, launched):
    """Persist the command's observed answer before any cleanup can refuse."""
    value = execution(path)
    value.update(exit_status=status,
                 state="completed" if launched and status < 128 else "interrupted")
    save(path, value)


def cleanup_failure(common, worktree, phase, detail):
    """Name retained resources without asserting that they are safe to modify."""
    common, worktree, key = paths(common, worktree)
    return {"phase": phase, "detail": detail, "owner": str(key) + ".owner",
            "worktree": str(worktree), "disposition": "permanent"}


def attach_cleanup(value, failure):
    """Preserve a completed primary result and every subsequent cleanup diagnosis."""
    if not isinstance(value, dict) or value.get("result") not in (
            "tests-failed", "gate-passed", "merged"):
        raise Refusal("cleanup refused without a completed child verdict")
    for name in ("tree", "detail"):
        if not isinstance(value.get(name), str) or not value[name]:
            raise Refusal(f"completed child verdict is missing {name}")
    if value["result"] != "merged" and not isinstance(value.get("log"), str):
        raise Refusal("completed gate verdict is missing its log")
    previous = value.get("cleanup_failure")
    if previous is not None:
        if not isinstance(previous, dict) or not isinstance(previous.get("detail"), str):
            raise Refusal("completed child verdict has invalid cleanup evidence")
        failure = dict(failure, detail=previous["detail"] + "\n" + failure["detail"])
    return dict(value, cleanup_failure=failure)


def main():
    """Initialize and validate the shell's private execution-evidence channel."""
    if sys.argv[1] == "cleanup":
        print(json.dumps(cleanup_failure(*sys.argv[2:])))
        return
    mode, path, tree, base, head = sys.argv[1:]
    identity = {"tree": tree, "base": base, "head": head}
    if mode == "init":
        save(path, dict(identity, version=1, attempt=Path(path).name,
                        owner=os.environ["STORYHOOK_VERIFIER_OWNER"], state="pending"))
        return
    if mode != "read":
        raise Refusal(f"unknown execution evidence operation: {mode}")
    value = execution(path)
    if any(value.get(key) != expected for key, expected in identity.items()):
        raise Refusal(f"execution evidence names a different candidate: {path}")
    status = value.get("exit_status")
    if value.get("state") != "completed" or type(status) is not int or not 0 <= status < 128:
        raise Refusal(f"gate did not complete normally: {path}")
    print(status)


if __name__ == "__main__":
    try:
        main()
    except (Refusal, OSError, ValueError, KeyError) as error:
        print(f"verifier-result: {error}", file=sys.stderr)
        sys.exit(1)
