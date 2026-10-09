"""Authoritative discovery of a pooled Rust battery, before any test executes.

Artifact lookup and listing share the pool's cancellation flag. Child output
uses regular files so a descendant cannot keep a pipe open past its owner.
"""

import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import threading
import time
from gate_cost import interval

POLL_SECONDS = 0.1
LIST_TIMEOUT = 120


class DiscoveryError(RuntimeError):
    """The battery cannot establish its runnable set."""


def process_tree(pid):
    """`pid` and all of its descendants, parents first."""
    tree = [pid]
    try:
        children = subprocess.run(
            ["pgrep", "-P", str(pid)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False
        ).stdout.split()
    except OSError:
        return tree
    for child in children:
        tree += process_tree(int(child))
    return tree


class Discovery:
    """Own artifact lookup and bounded, cancellable libtest listing."""

    def __init__(self, env, cancelled, list_timeout=LIST_TIMEOUT):
        self.env, self.cancelled, self.list_timeout = env, cancelled, list_timeout
        self.stopped = threading.Event()

    def stop(self):
        """Prevent further listing after another job's discovery failed."""
        self.stopped.set()

    def run(self, command, context, timeout=None):
        """Capture one child; a refusal always names its command and diagnostics."""
        description = f"{context}: {shlex.join(command)}"
        if self.cancelled() or self.stopped.is_set():
            raise DiscoveryError(f"{description}: cancelled before launch")
        try:
            with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
                child = subprocess.Popen(command, env=self.env, stdin=subprocess.DEVNULL,
                                         stdout=output, stderr=errors)
                started = time.monotonic()
                reason = None
                while child.poll() is None:
                    if self.cancelled() or self.stopped.is_set():
                        reason = "cancelled"
                    elif timeout is not None and time.monotonic() - started >= timeout:
                        reason = f"timed out after {timeout:g}s"
                    if reason:
                        # Discovery executes no test bodies or cleanup. Kill the
                        # owned tree and reap its leader before returning failure.
                        for pid in reversed(process_tree(child.pid)):
                            try:
                                os.kill(pid, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                        child.wait()
                        break
                    try:
                        child.wait(timeout=POLL_SECONDS)
                    except subprocess.TimeoutExpired:
                        pass  # Recheck cancellation and the listing deadline.
                output.seek(0)
                errors.seek(0)
                stdout = output.read().decode(errors="replace")
                stderr = errors.read().decode(errors="replace")
                if reason or child.returncode:
                    raise DiscoveryError(
                        f"{description}: {reason or f'exited {child.returncode}'}\n{stderr}{stdout}"
                    )
                if self.cancelled() or self.stopped.is_set():
                    raise DiscoveryError(f"{description}: cancelled")
                return stdout
        except OSError as error:
            raise DiscoveryError(f"{description}: {error}") from error

    @interval("discovery", "release gate/rust-list")
    def count_tests(self, executable, libtest_args):
        """Count runnable cases using libtest's own selection semantics."""
        def listed(args):
            output = self.run([executable, "--list", *args], "listing tests", self.list_timeout)
            return sum(line.endswith((": test", ": benchmark")) for line in output.splitlines())

        total = listed(libtest_args)
        if "--ignored" not in libtest_args and "--include-ignored" not in libtest_args:
            ignored = listed([*libtest_args, "--ignored"])
            if ignored > total:
                raise DiscoveryError(
                    f"{executable}: ignored discovery ({ignored}) exceeds all discovery ({total})"
                )
            total -= ignored
        return total

    def executables(self, jobs, cargo_extra):
        """Resolve every selected target or fail before any test starts."""
        found = {}
        groups = {}
        for job in jobs:
            groups.setdefault(job.package, []).append(job)
        for package, members in groups.items():
            command = [str(Path(__file__).with_name("managed-cargo.sh")), "test", "--no-run", "--message-format=json", "-p", package, *cargo_extra]
            for job in members:
                command += job.selector()
            output = self.run(command, f"artifact lookup for {package}")
            for line in output.splitlines():
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(message, dict) or message.get("reason") != "compiler-artifact":
                    continue
                executable = message.get("executable")
                if not executable:
                    continue
                target = message.get("target", {})
                # Cargo also emits companion application binaries. Their names
                # can match a test target, but they are never test harnesses.
                kind = next((kind for kind in ("lib", "test") if kind in target.get("kind", [])), None)
                if kind is not None:
                    found[(package, kind, target.get("name"))] = executable
            for job in members:
                if (job.package, job.kind, job.name) not in found:
                    raise DiscoveryError(
                        f"artifact lookup: no test executable for {job.key}; command: {shlex.join(command)}\n{output}"
                    )
        return found
