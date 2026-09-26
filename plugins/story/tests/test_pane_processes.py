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
import probe_budget  # noqa: E402  (the same module object the helpers use)

CENSUS = ["ps", "-axo", "pid=,ppid=,stat=,lstart="]
# The bare per-probe bound SH-766 retires; a census slower than this failed.
RETIRED_PROBE_BOUND = 5
# Fixture processes publish their PID within this many seconds at idle.
FIXTURE_START_SECONDS = 5


def stopped(pid):
    """Report whether ps shows this process stopped by a signal."""
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout
    return state.strip().startswith("T")


def install_slow_census(bindir, scratch):
    """Put a ps on PATH that delays the census once, as machine load does.

    Writing seconds to the returned file delays the next census, and only
    that one; every other ps call runs the real ps at once.
    """
    real = shutil.which("ps")
    delay = scratch / "census-delay"
    shim = bindir / "ps"
    shim.write_text("#!/bin/sh\n"
                    f"if [ \"$*\" = {shlex.quote(' '.join(CENSUS[1:]))} ] && [ -f {shlex.quote(str(delay))} ]; then\n"
                    f"  seconds=$(cat {shlex.quote(str(delay))}); rm -f {shlex.quote(str(delay))}; sleep \"$seconds\"\n"
                    "fi\n"
                    f"exec {shlex.quote(real)} \"$@\"\n")
    shim.chmod(0o755)
    return delay


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
        self.delay = install_slow_census(bindir, self.path)
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

    def test_a_census_slower_than_the_retired_bound_completes(self):
        """The helper entry point gives a slow census its budget, not a bare 5 s (SH-766)."""
        self.delay.write_text(str(RETIRED_PROBE_BOUND + 0.5))
        started = time.monotonic()
        result = subprocess.run([sys.executable, str(LIB / "stop-dispatch-pane.py"), self.pane, str(self.pid), self.start],
                                capture_output=True, text=True, timeout=probe_budget.BUDGET_SECONDS * 2)
        self.assertEqual(result.stdout.strip(), '{"ok": true}', result.stdout + result.stderr)
        self.assertFalse(self.delay.exists(), "the census was never delayed")
        self.assertGreater(time.monotonic() - started, RETIRED_PROBE_BOUND)
        self.assertFalse(self.running(self.child, self.child_start), "the grouped descendant survived")


class BudgetTests(unittest.TestCase):
    """One deadline per operation bounds every probe inside it (SH-766)."""

    def setUp(self):
        root = tempfile.TemporaryDirectory(prefix="probe-budget-", dir="/tmp")
        self.addCleanup(root.cleanup)
        self.path = Path(root.name)
        bindir = self.path / "bin"
        bindir.mkdir()
        self.delay = install_slow_census(bindir, self.path)
        environment = patch.dict(os.environ, {"PATH": f"{bindir}:{os.environ['PATH']}"})
        environment.start()
        self.addCleanup(environment.stop)

    def test_a_probe_that_outlives_its_budget_names_its_evidence(self):
        """The timeout says which probe, how long it had, and how loaded the machine was."""
        self.delay.write_text("3")
        with probe_budget.operation(budget=1):
            with self.assertRaises(probe_budget.ProbeTimeout) as raised:
                proc.processes()
        error = raised.exception
        self.assertIsInstance(error, subprocess.TimeoutExpired, "existing handlers must still catch it")
        self.assertLessEqual(error.timeout, 1)
        text = str(error)
        for evidence in (" ".join(CENSUS), "1s operation budget", "load average"):
            self.assertIn(evidence, text)

    def test_a_spent_budget_refuses_before_spawning(self):
        """No probe starts once its operation has no time left."""
        marker = self.path / "spawned"
        with probe_budget.operation(budget=0.01):
            time.sleep(0.05)
            self.assertEqual(probe_budget.remaining(), 0)
            with self.assertRaises(probe_budget.ProbeTimeout) as raised:
                proc.run("sh", "-c", f"touch {shlex.quote(str(marker))}")
        self.assertEqual(raised.exception.timeout, 0)
        self.assertFalse(marker.exists(), "a probe ran after its budget was spent")

    def test_a_nested_operation_keeps_the_outer_deadline(self):
        """An inner operation cannot extend the time its caller granted."""
        with probe_budget.operation(budget=2):
            with probe_budget.operation(budget=probe_budget.BUDGET_SECONDS):
                self.assertLessEqual(probe_budget.remaining(), 2)

    def test_a_probe_outside_an_operation_gets_one_whole_budget(self):
        """Library callers without an operation still get a finite, named bound."""
        self.assertEqual(probe_budget.remaining(), probe_budget.BUDGET_SECONDS)


if __name__ == "__main__":
    unittest.main()
