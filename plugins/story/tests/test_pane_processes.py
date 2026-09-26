#!/usr/bin/env python3
"""Real process-tree regressions for the shared pane helper, stop-dispatch-pane.py."""
import importlib.util
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
LIB = Path(__file__).resolve().parents[1] / "lib"
sys.path.insert(0, str(LIB))
spec = importlib.util.spec_from_file_location("pane_processes", LIB / "stop-dispatch-pane.py")
proc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proc)

CENSUS = ["ps", "-axo", "pid=,ppid=,stat=,lstart="]
# Fixture processes publish their PID within this many seconds at idle.
FIXTURE_START_SECONDS = 5


def stopped(pid):
    """Report whether ps shows this process stopped by a signal."""
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout
    return state.strip().startswith("T")


class PaneFixture(unittest.TestCase):
    """One private tmux server whose pane runs a descendant in its own process group.

    tmux continues a pane process that it sees stop, and that SIGCONT reaches
    the pane's whole process group. A descendant in its own group, the shape
    of a gate child, is the process that stays frozen when cleanup fails.
    """

    def setUp(self):
        root = tempfile.TemporaryDirectory(prefix="pane-processes-", dir="/tmp")
        self.addCleanup(root.cleanup)
        self.path = Path(root.name)
        self.socket = self.path / "tmux.sock"
        tmux = shutil.which("tmux")
        self.assertTrue(tmux, "tmux is required; this regression must not skip")
        bindir = self.path / "bin"
        bindir.mkdir()
        wrapper = bindir / "tmux"
        # The helper addresses the caller's server; pin it to this private one.
        wrapper.write_text(f"#!/bin/sh\nexec {shlex.quote(tmux)} -S {shlex.quote(str(self.socket))} \"$@\"\n")
        wrapper.chmod(0o755)
        environment = patch.dict(os.environ, {"PATH": f"{bindir}:{os.environ['PATH']}"})
        environment.start()
        self.addCleanup(environment.stop)
        os.environ.pop("TMUX_PANE", None)
        self.addCleanup(self.tmux, "kill-server", check=False)
        grouped = self.path / "grouped"
        script = self.path / "grouped.py"
        script.write_text("import os, pathlib, time\nos.setpgid(0, 0)\n"
                          f"pathlib.Path({str(grouped)!r}).write_text(str(os.getpid()))\n"
                          "time.sleep(120)\n")
        self.tmux("new-session", "-d", "-s", "fixture", "-n", "OTHER", "sleep 120")
        self.tmux("new-window", "-n", "SH-1", "-c", str(self.path), f"sh -c '{sys.executable} {script} & wait'")
        deadline = time.monotonic() + FIXTURE_START_SECONDS
        while not grouped.exists() or not grouped.read_text().strip():
            if time.monotonic() > deadline:
                self.fail("grouped fixture child did not start")
            time.sleep(0.01)
        self.child = int(grouped.read_text())
        self.child_start = proc.process_identity(self.child)["start"]
        self.addCleanup(self.kill_exact, self.child, self.child_start)
        self.pane, pid = self.tmux("display-message", "-p", "-t", "fixture:SH-1", "#{pane_id} #{pane_pid}").split()
        self.pid = int(pid)
        self.start = proc.process_identity(self.pid)["start"]

    def tmux(self, *args, check=True):
        """Run tmux on this case's private server."""
        return subprocess.run(["tmux", *args], capture_output=True, text=True, check=check).stdout

    def running(self, pid, start):
        """Report whether this exact incarnation still runs."""
        try:
            return proc.process_identity(pid)["start"] == start
        except OSError:
            return False  # Exited, or reaped and gone.

    def kill_exact(self, pid, start):
        """Kill a fixture process only while it is still the captured incarnation."""
        if self.running(pid, start):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass  # Exit won the race.


class StopTests(PaneFixture):
    """stop() freezes, identifies and ends exactly the pane's startup tree."""

    def test_stop_ends_the_pane_and_every_descendant(self):
        """The success path removes the pane and its separately grouped descendant."""
        proc.stop(self.pane, self.pid, self.start)
        self.assertNotIn(self.pane, self.tmux("list-panes", "-a", "-F", "#{pane_id}"))
        self.assertFalse(self.running(self.child, self.child_start), "the grouped descendant survived")

    def test_failed_census_after_freezing_resumes_the_startup_tree(self):
        """A census that fails under load must not leave the startup tree frozen (SH-766)."""
        census, kill = proc.processes, os.kill
        frozen = []

        def freeze_aware_kill(pid, sig):
            if sig == signal.SIGSTOP:
                frozen.append(pid)
            kill(pid, sig)

        def slow_census():
            if self.child in frozen:
                raise subprocess.TimeoutExpired(CENSUS, 5)
            return census()

        with patch.object(proc, "processes", side_effect=slow_census), \
                patch.object(proc.os, "kill", side_effect=freeze_aware_kill):
            with self.assertRaises(subprocess.TimeoutExpired):
                proc.stop(self.pane, self.pid, self.start)
        self.assertIn(self.child, frozen, "the case never froze the grouped descendant")
        self.assertFalse(stopped(self.child), "the frozen descendant stayed stopped after a failed census")
        self.assertIn(self.pane, self.tmux("list-panes", "-a", "-F", "#{pane_id}"),
                      "an uncertain stop must preserve the pane")


if __name__ == "__main__":
    unittest.main()
