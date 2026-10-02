"""SH-805: real process groups must not outlive the reporter test owner."""

import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

from load_grace import contention, patience

SCRIPT = Path(__file__).resolve().parents[1] / "test-browser-launch-reporter.py"


class ReporterCleanupTests(unittest.TestCase):
    """Exercise actual cancellation, timeout, exit, and inherited pipe lifetimes."""

    def run_case(self, mode):
        """A nested runner deliberately leaves a descendant holding its output pipe."""
        budget = patience(10, contention())
        with tempfile.TemporaryDirectory(prefix="SH-805-cleanup-", dir="/tmp") as scratch:
            root = Path(scratch)
            marker = root / "descendant"
            child = root / "child.py"
            child.write_text(
                "import os, pathlib, subprocess, sys, time\n"
                "p = subprocess.Popen([sys.executable, '-c', "
                "\"import os, pathlib, time; p = pathlib.Path(\" + repr(sys.argv[1]) + "
                "\"); p.with_suffix('.tmp').write_text(str(os.getppid()) + ' ' + str(os.getpid())); "
                "p.with_suffix('.tmp').replace(p); time.sleep(3600)\"])\n"
                "while not pathlib.Path(sys.argv[1]).exists(): time.sleep(0.01)\n"
                "print('nested output', flush=True)\n"
                "if sys.argv[2] == 'success': sys.exit(0)\n"
                "if sys.argv[2] == 'failure': sys.exit(7)\n"
                "time.sleep(3600)\n"
            )
            driver = (
                "import runpy, sys\n"
                f"m = runpy.run_path({str(SCRIPT)!r})\n"
                "try:\n"
                f" r = m['run_owned']([sys.executable, {str(child)!r}, {str(marker)!r}, {mode!r}], "
                f"timeout={budget if mode == 'timeout' else budget * 3!r}, watch_parent={mode == 'eof'!r})\n"
                " print('result', r.returncode, r.stdout, flush=True)\n"
                "except BaseException as error:\n"
                " print(type(error).__name__, str(error), flush=True); sys.exit(3)\n"
            )
            owner = subprocess.Popen([sys.executable, "-B", "-c", driver], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            pid = None
            child_pid = None
            try:
                deadline = time.monotonic() + budget
                while not marker.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(marker.exists(), "nested runner did not publish readiness")
                child_pid, pid = map(int, marker.read_text().split())
                if mode in ("term", "interrupt"):
                    owner.send_signal(signal.SIGTERM if mode == "term" else signal.SIGINT)
                if mode == "eof":
                    owner.stdin.close()
                    owner.stdin = None
                # Keep the pipe open in other cases: communicate() would itself send EOF.
                owner.wait(timeout=budget * 2)
                output = owner.stdout.read()
                if mode in ("success", "failure"):
                    self.assertEqual(owner.returncode, 0, output)
                    self.assertIn("result " + ("0" if mode == "success" else "7"), output)
                    self.assertIn("nested output", output)
                else:
                    self.assertEqual(owner.returncode, 3, output)
                    self.assertIn("TimeoutExpired" if mode == "timeout" else "ReporterCancelled", output)
                deadline = time.monotonic() + budget
                while self.alive(pid) and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertFalse(self.alive(pid), f"descendant {pid} survived {mode}: {output}")
            finally:
                if owner.poll() is None:
                    owner.kill()
                owner.communicate()
                if pid is not None and self.alive(pid):
                    os.kill(pid, signal.SIGKILL)
                if child_pid is not None and self.alive(child_pid):
                    os.kill(child_pid, signal.SIGKILL)

    @staticmethod
    def alive(pid):
        """A Linux orphan awaiting init's reap is dead, even while its PID exists."""
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        stat = Path(f"/proc/{pid}/stat")
        return not (stat.exists() and stat.read_text().split(") ", 1)[1].startswith("Z"))

    def test_success_cleans_descendants_holding_output(self):
        """Even a successful parent cannot leave a pipe-holding worker alive."""
        self.run_case("success")

    def test_failure_preserves_status_and_output(self):
        """Cleanup preserves the nested command's original failed result."""
        self.run_case("failure")

    def test_timeout_cleans_descendants(self):
        """The nested deadline terminates every process in the owned group."""
        self.run_case("timeout")

    def test_term_cleans_descendants(self):
        """Node's default timeout/abort signal triggers cleanup before exit."""
        self.run_case("term")

    def test_interrupt_cleans_descendants(self):
        """Interactive interruption follows the same owned cleanup path."""
        self.run_case("interrupt")

    def test_parent_pipe_loss_cleans_descendants(self):
        """A dead outer worker cannot leave its Python and Node children running."""
        self.run_case("eof")


if __name__ == "__main__":
    unittest.main()
