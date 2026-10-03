"""Gate evidence from real work boundaries, never timer heartbeats or authority."""

from contextlib import contextmanager
import datetime
import json
import os
from pathlib import Path
import platform
import resource
import shutil
import subprocess
import sys
import time
import uuid


def emit(value):
    """Append one complete local-file record; refuse a missing or unwritable journal."""
    journal = os.environ.get("STORYHOOK_GATE_PROGRESS")
    if not journal:
        return
    data = (json.dumps(value, ensure_ascii=True, separators=(",", ":")) + "\n").encode()
    fd = os.open(journal, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW)
    try:
        if os.write(fd, data) != len(data):
            raise OSError(f"short gate cost write to {journal}")
    finally:
        os.close(fd)


def boundary(event, phase, identity, path):
    """Record a monotonic endpoint paired with a UTC observation."""
    emit(dict(kind="cost", event=event, phase=phase, id=identity, path=path,
              monotonic_ns=time.monotonic_ns(),
              at=datetime.datetime.now(datetime.timezone.utc).isoformat()))


@contextmanager
def interval(phase, path):
    """Measure work until normal return or exception; process death leaves an open span."""
    identity = uuid.uuid4().hex
    boundary("start", phase, identity, path)
    try:
        yield
    finally:
        boundary("end", phase, identity, path)


def tool_version(command):
    """Report a bounded tool probe, retaining failure as unknown with its diagnostic."""
    executable = shutil.which(command[0])
    if executable is None:
        return dict(version=None, diagnostic=f"{command[0]} is not on PATH")
    try:
        result = subprocess.run([executable, *command[1:]], capture_output=True,
                                text=True, timeout=5, check=False)
        if result.returncode:
            return dict(executable=executable, version=None,
                        diagnostic=f"exit {result.returncode}: {result.stderr[:2048]}")
        return dict(executable=executable, version=result.stdout.strip()[:4096])
    except (OSError, subprocess.TimeoutExpired) as error:
        return dict(executable=executable, version=None, diagnostic=str(error))


def context(command):
    """Capture the scheduled launch environment without inspecting secrets or changing policy."""
    if not os.environ.get("STORYHOOK_GATE_PROGRESS"):
        return
    toolchain = dict(platform=platform.platform(), machine=platform.machine(),
                     python=dict(executable=sys.executable, version=sys.version),
                     git=tool_version(["git", "--version"]))
    if Path("Cargo.toml").is_file():
        toolchain["rustc"] = tool_version(["rustc", "-vV"])
        toolchain["cargo"] = tool_version(["cargo", "--version"])
    if Path("package.json").is_file():
        toolchain["node"] = tool_version(["node", "--version"])
        toolchain["npm"] = tool_version(["npm", "--version"])
    inputs = {key: os.environ.get("STORYHOOK_GATE_COST_" + key.upper())
              for key in ("head", "base", "tree")}
    inputs.update(contract=dict(argv=command), toolchain=toolchain,
                  resources=dict(nofile=resource.getrlimit(resource.RLIMIT_NOFILE),
                                 processes=resource.getrlimit(resource.RLIMIT_NPROC),
                                 cpu_count=os.cpu_count(), nice=os.getpriority(os.PRIO_PROCESS, 0),
                                 scheduling_argv=json.loads(os.environ.get("STORYHOOK_GATE_COST_CLASS", "null")),
                                 policy="existing verifier scheduling; no new admission governor"),
                  cache=dict(build_artifacts="unmeasured", validation="per-leg receipt evidence"))
    emit(dict(kind="context", inputs=inputs))
