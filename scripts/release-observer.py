#!/usr/bin/env python3
"""Observe release tags and host capabilities without certifying or shipping bytes."""
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
import time

SCRIPTS = Path(__file__).resolve().parent
INTERVAL_SECONDS = 60 * 60
STALE_SECONDS = 2 * INTERVAL_SECONDS
TAG_PREFIX = "refs/release-observer/tags/"


def git(root, *args):
    """Read Git output, preserving failures and their diagnostic context."""
    return subprocess.check_output(
        ["git", "-C", str(root), *args], stderr=subprocess.PIPE, text=True
    ).strip()


def state_dir(root):
    """Resolve observation storage shared by this repository's worktrees."""
    common = Path(git(root, "rev-parse", "--git-common-dir"))
    return (root / common).resolve() / "storyhook/release-observer"


def process_started(pid):
    """Read process identity so a reused PID cannot disguise an interrupted pass."""
    result = subprocess.run(
        ["ps", "-p", str(pid), "-o", "lstart="], capture_output=True, text=True
    )
    return result.stdout.strip() if result.returncode == 0 else ""


def save(directory, record):
    """Publish one complete record atomically; readers never see a partial JSON write."""
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=directory, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(record, stream)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    try:
        os.replace(temporary, directory / "latest.json")
    finally:
        temporary.unlink(missing_ok=True)


def audit(checkout, log):
    """Validate every release-shaped remote tag through the shared commit resolver."""
    refs = git(checkout, "for-each-ref", "--format=%(refname)", TAG_PREFIX).splitlines()
    failed = False
    for ref in refs:
        version = ref[len(TAG_PREFIX):]
        valid = subprocess.run(
            ["bash", "-c", 'source "$1"; release_version_is_valid "$2"',
             "release-observer", str(SCRIPTS / "release-targets.sh"), version],
            stdout=log, stderr=log,
        )
        if valid.returncode == 1:
            continue
        if valid.returncode != 0:
            raise ValueError("release version predicate failed")
        try:
            actual = git(checkout, "rev-parse", "--verify", ref + "^{commit}")
            expected = subprocess.check_output(
                ["bash", str(SCRIPTS / "release-tag-commit.sh"), version, actual],
                cwd=checkout, stderr=subprocess.STDOUT, text=True,
            ).strip()
            if expected != actual:
                failed = True
                log.write(f"INVALID {version}: actual={actual} expected={expected}\n")
            else:
                log.write(f"OK {version}: {actual}\n")
        except subprocess.CalledProcessError as error:
            failed = True
            log.write(f"INVALID {version}: {error.stderr or error.output}\n")
    log.flush()
    return int(failed)


def watch(root):
    """Run one observation; callers serialize passes with machine-lock.sh."""
    directory = state_dir(root)
    directory.mkdir(parents=True, exist_ok=True)
    record = {
        "schema": 1, "state": "running", "started": int(time.time()),
        "finished": None, "pid": os.getpid(),
        "process_started": process_started(os.getpid()),
        "commit": None, "tree": None, "audit": None, "preflight": None,
    }
    with tempfile.NamedTemporaryFile(mode="w", prefix="pass-", suffix=".log",
                                     dir=directory, delete=False) as log:
        record["log"] = str(Path(log.name).resolve())
        save(directory, record)
        try:
            remote = git(root, "remote", "get-url", "origin")
            if not (":" in remote or Path(remote).is_absolute()):
                remote = str((root / remote).resolve())
            checkout = directory / "checkout"
            if not checkout.exists():
                subprocess.run(["git", "init", "-q", str(checkout)], check=True,
                               stdout=log, stderr=log)
            # Explicit namespaces prevent auto-follow from populating refs/tags.
            # Forced updates affect only this observer's private remote snapshot.
            subprocess.run(
                ["git", "-C", str(checkout), "fetch", "--quiet", "--no-tags", "--prune",
                 remote, "+refs/heads/main:refs/remotes/origin/main",
                 "+refs/tags/*:" + TAG_PREFIX + "*"],
                check=True, stdout=log, stderr=log,
            )
            record["commit"] = git(checkout, "rev-parse", "refs/remotes/origin/main")
            record["tree"] = git(checkout, "rev-parse", record["commit"] + "^{tree}")
            save(directory, record)
            # A tag failure must not prevent exercising the independent host path.
            try:
                record["audit"] = audit(checkout, log)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                record["audit"] = 1
                log.write(f"tag audit failed: {error}\n")
            log.flush()
            if git(checkout, "status", "--porcelain", "--untracked-files=no"):
                raise ValueError(f"observer checkout has tracked edits: {checkout}; refusing to overwrite them")
            subprocess.run(["git", "-C", str(checkout), "checkout", "--quiet", "--detach",
                            record["commit"]], check=True, stdout=log, stderr=log)
            record["preflight"] = subprocess.run(
                ["bash", "scripts/build-release-assets.sh", "--check"],
                cwd=checkout, stdout=log, stderr=log,
            ).returncode
            if git(checkout, "status", "--porcelain", "--untracked-files=no"):
                raise ValueError(f"preflight modified tracked files in {checkout}")
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            record["error"] = f"observation failed: {error} {getattr(error, 'stderr', '') or ''}"
            log.write(record["error"] + "\n")
        record["finished"] = int(time.time())
        record["state"] = "finished"
        save(directory, record)
    state, message = status(root)
    print(message, file=sys.stderr)
    return 0 if record["audit"] == 0 and record["preflight"] == 0 and not record.get("error") else 1


def status(root):
    """Report observation health without fetching, probing, or writing anything."""
    path = state_dir(root) / "latest.json"
    if not path.exists():
        return "missing", "release-status: MISSING — no release observation; run make release-watch"
    try:
        record = json.loads(path.read_text())
        required = {"schema", "state", "started", "finished", "pid", "process_started",
                    "commit", "tree", "audit", "preflight", "log"}
        if (not isinstance(record, dict) or not required <= record.keys()
                or type(record["schema"]) is not int or record["schema"] != 1):
            raise ValueError("invalid observation schema")
        if type(record["started"]) is not int or record["started"] > time.time():
            raise ValueError("invalid observation start time")
        if not isinstance(record["log"], str) or not Path(record["log"]).is_file():
            raise ValueError("missing diagnostic log")
        if record["state"] == "running":
            if (type(record["pid"]) is int and record["pid"] > 0
                    and record["process_started"]
                    and record["process_started"] == process_started(record["pid"])):
                state = "running"
            else:
                state = "failed"
        elif record["state"] == "finished":
            if type(record["finished"]) is not int or not record["started"] <= record["finished"] <= time.time():
                raise ValueError("invalid observation finish time")
            if (type(record["audit"]) is not int or type(record["preflight"]) is not int
                    or record["audit"] != 0 or record["preflight"] != 0 or record.get("error")):
                state = "failed"
            else:
                for key in ("commit", "tree"):
                    if (not isinstance(record[key], str)
                            or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", record[key])):
                        raise ValueError(f"invalid observation {key}")
                tree = git(root, "rev-parse", "refs/remotes/origin/main^{tree}")
                state = "stale" if (tree != record["tree"] or
                                     time.time() - record["finished"] > STALE_SECONDS) else "successful"
        else:
            raise ValueError("unknown observation state")
        age = int(time.time()) - (record["finished"] or record["started"])
        return state, (f"release-status: {state.upper()} age={age}s "
                       f"commit={record['commit']} audit={record['audit']} "
                       f"preflight={record['preflight']} log={record['log']}")
    except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
        return "failed", f"release-status: FAILED — cannot validate {path}: {error}"


def launchd(root):
    """Render, but never install, hourly configuration for a durable checkout."""
    root = root.resolve()
    if not (root / ".git").is_dir():
        raise ValueError("launchd requires a durable standalone checkout, never a linked worktree")
    label = "io.mikey.storyhook.release-observer"
    return plistlib.dumps({
        "Label": label,
        "ProgramArguments": ["/bin/bash", str(root / "scripts/release-watch.sh")],
        "WorkingDirectory": str(root),
        "EnvironmentVariables": {"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
        "RunAtLoad": True,
        "StartCalendarInterval": {"Minute": 0},
        "StandardOutPath": str(Path.home() / "Library/Logs" / (label + ".log")),
        "StandardErrorPath": str(Path.home() / "Library/Logs" / (label + ".log")),
    })


def main():
    """Dispatch the internal observer command; public watch uses the lock wrapper."""
    if len(sys.argv) != 2 or sys.argv[1] not in {"watch", "status", "launchd"}:
        raise ValueError("usage: release-observer.py watch|status|launchd")
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel"))
    if sys.argv[1] == "watch":
        return watch(root)
    if sys.argv[1] == "launchd":
        sys.stdout.buffer.write(launchd(root))
        return 0
    state, message = status(root)
    print(message, file=sys.stderr)
    return 0 if state == "successful" else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.SubprocessError) as failure:
        print(f"release-observer: {failure}", file=sys.stderr)
        sys.exit(2)
