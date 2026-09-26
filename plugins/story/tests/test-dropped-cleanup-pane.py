#!/usr/bin/env python3
"""Real process-tree and journal recovery tests for dropped workspace cleanup."""
import importlib.util
import json
import os
from pathlib import Path
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
spec = importlib.util.spec_from_file_location("cleanup", LIB / "dropped-cleanup-pane.py")
cleanup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cleanup)


def stopped(pid):
    """Report whether ps shows this process stopped by a signal."""
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout
    return state.strip().startswith("T")


class CleanupTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="dropped-pane-", dir="/tmp")
        self.path = Path(self.root.name)
        self.socket = str(self.path / "tmux.sock")
        self.journal = self.path / "processes.json"
        self.tmux("new-session", "-d", "-s", "fixture", "-n", "OTHER", "sleep 120")
        child_file = self.path / "child"
        self.tmux("new-window", "-n", "SH-1", "-c", str(self.path),
                  f"sh -c 'sleep 120 & echo $! > {child_file}; wait'")
        deadline = time.monotonic() + 5
        while not child_file.exists() or not child_file.read_text().strip():
            if time.monotonic() > deadline:
                self.fail("fixture child did not start")
            time.sleep(0.01)
        self.child = int(child_file.read_text())
        self.child_start = cleanup.proc.process_identity(self.child)["start"]
        window, pane, pid = self.tmux("display-message", "-p", "-t", "fixture:SH-1",
                                     "#{window_id} #{pane_id} #{pane_pid}").split()
        self.target = {"socket": self.socket, "name": "SH-1", "window": window,
                       "pane": pane, "pid": pid,
                       "start": cleanup.proc.process_identity(int(pid))["start"]}

    def tmux(self, *args):
        return cleanup.proc.run("tmux", "-S", self.socket, *args)

    def tearDown(self):
        subprocess.run(["tmux", "-S", self.socket, "kill-server"], capture_output=True, check=False)
        try:
            if cleanup.proc.process_identity(self.child)["start"] == self.child_start:
                os.kill(self.child, signal.SIGKILL)
        except OSError:
            pass  # The successful cleanup already removed this process.
        self.root.cleanup()

    def known(self):
        table = cleanup.proc.processes()
        return {pid: (*table[pid], cleanup.proc.process_identity(pid)["start"])
                for pid in cleanup.proc.descendants(table, int(self.target["pid"]))}

    def assert_quiescent(self):
        self.assertEqual(self.tmux("list-windows", "-a", "-F", "#{window_name}"), "OTHER")
        table = cleanup.proc.processes()
        self.assertNotIn(self.child, table)
        record = json.loads(self.journal.read_text())
        self.assertEqual(record["phase"], "complete")
        self.assertIn(str(self.child), record["owned"])

    def test_kills_captured_descendants_and_retries_without_effects(self):
        cleanup.stop(self.target, self.journal)
        self.assert_quiescent()
        cleanup.stop(self.target, self.journal)
        self.assert_quiescent()

    def test_census_retry_refreezes_previously_resumed_processes(self):
        owned = self.known()
        cleanup.save(self.journal, {"target": self.target, "phase": "census", "owned": owned})
        cleanup.stop(self.target, self.journal)
        self.assert_quiescent()

    def test_restart_after_window_closed_still_reaps_its_descendants(self):
        owned = self.known()
        cleanup.save(self.journal, {"target": self.target, "phase": "terminating", "owned": owned})
        cleanup.proc.signal_known(owned, signal.SIGSTOP)
        self.tmux("kill-window", "-t", self.target["window"])
        cleanup.stop(self.target, self.journal)
        self.assert_quiescent()

    def test_wrong_start_token_never_signals_or_closes_the_pane(self):
        target = {**self.target, "start": "wrong-incarnation"}
        with self.assertRaises(cleanup.proc.CleanupError):
            cleanup.stop(target, self.journal)
        self.assertIn("SH-1", self.tmux("list-windows", "-a", "-F", "#{window_name}"))
        self.assertEqual(cleanup.proc.process_identity(self.child)["start"], self.child_start)
        self.assertFalse(self.journal.exists())

    def test_duplicate_window_prevents_any_signal(self):
        self.tmux("new-window", "-n", "SH-1", "sleep 120")
        with self.assertRaises(cleanup.proc.CleanupError):
            cleanup.stop(self.target, self.journal)
        self.assertEqual(cleanup.proc.process_identity(self.child)["start"], self.child_start)
        self.assertFalse(self.journal.exists())

    def test_completed_journal_never_closes_a_replacement(self):
        cleanup.stop(self.target, self.journal)
        self.tmux("new-window", "-n", "SH-1", "sleep 120")
        with self.assertRaises(cleanup.proc.CleanupError):
            cleanup.stop(self.target, self.journal)
        self.assertIn("SH-1", self.tmux("list-windows", "-a", "-F", "#{window_name}"))

    def test_failed_census_after_freezing_resumes_the_closure(self):
        """A census that fails under load must not leave the pane frozen (SH-766).

        The pane runs a descendant in its own process group, the shape of a
        gate child, which tmux does not continue when it sees the pane process
        stop. The first census after that descendant is frozen fails, as a
        slow ps does under load. Settled capture errors must resume the pinned
        closure, and resuming must not depend on another census.
        """
        grouped = self.path / "grouped"
        script = self.path / "grouped.py"
        script.write_text("import os, pathlib, time\nos.setpgid(0, 0)\n"
                          f"pathlib.Path({str(grouped)!r}).write_text(str(os.getpid()))\n"
                          "time.sleep(120)\n")
        self.tmux("kill-window", "-t", self.target["window"])
        self.tmux("new-window", "-n", "SH-1", "-c", str(self.path),
                  f"sh -c '{sys.executable} {script} & wait'")
        deadline = time.monotonic() + 5
        while not grouped.exists() or not grouped.read_text().strip():
            if time.monotonic() > deadline:
                self.fail("grouped fixture child did not start")
            time.sleep(0.01)
        child = int(grouped.read_text())
        child_start = cleanup.proc.process_identity(child)["start"]
        self.addCleanup(self.kill_exact, child, child_start)
        window, pane, pid = self.tmux("display-message", "-p", "-t", "fixture:SH-1",
                                     "#{window_id} #{pane_id} #{pane_pid}").split()
        target = {**self.target, "window": window, "pane": pane, "pid": pid,
                  "start": cleanup.proc.process_identity(int(pid))["start"]}
        census, kill = cleanup.proc.processes, os.kill
        frozen = []

        def freeze_aware_kill(pid, sig):
            if sig == signal.SIGSTOP:
                frozen.append(pid)
            kill(pid, sig)

        def slow_census():
            if child in frozen:
                raise subprocess.TimeoutExpired(["ps", "-axo", "pid=,ppid=,stat=,lstart="], 5)
            return census()

        with patch.object(cleanup.proc, "processes", side_effect=slow_census), \
                patch.object(cleanup.os, "kill", side_effect=freeze_aware_kill):
            with self.assertRaises(subprocess.TimeoutExpired):
                cleanup.stop(target, self.journal)
        self.assertIn(child, frozen, "the case never froze the grouped descendant")
        self.assertFalse(stopped(child), "the frozen descendant stayed stopped after a failed census")
        self.assertEqual(json.loads(self.journal.read_text())["phase"], "census")
        self.assertIn("SH-1", self.tmux("list-windows", "-a", "-F", "#{window_name}"))

    def kill_exact(self, pid, start):
        """Kill a fixture process only while it is still the captured incarnation."""
        try:
            if cleanup.proc.process_identity(pid)["start"] == start:
                os.kill(pid, signal.SIGKILL)
        except OSError:
            pass  # Already gone.

if __name__ == "__main__":
    unittest.main()
