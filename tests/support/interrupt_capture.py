"""Pin interruption capture races at the native process-probe boundary."""

import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import sys
import unittest
from unittest.mock import patch


library = Path(__file__).resolve().parents[2] / "plugins/story/lib"
sys.path.insert(0, str(library))
spec = importlib.util.spec_from_file_location("interrupt_agent", library / "interrupt-agent.py")
interrupt = importlib.util.module_from_spec(spec)
handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGHUP)}
spec.loader.exec_module(interrupt)
for sig, handler in handlers.items():
    signal.signal(sig, handler)


class CaptureTests(unittest.TestCase):
    """Use the production capture loop with controlled kernel probe outcomes."""

    def capture(self, error):
        """Exit a discovered child between the process census and identity probe."""
        root, child = 1001, 1002
        first = {root: (1, "root-start"), child: (root, "child-start")}
        later = {root: first[root]}
        owned = {}

        def identity(pid):
            if pid == child:
                raise error
            self.assertEqual(pid, root)
            return {"start": "native-root"}

        with patch.object(interrupt, "processes", side_effect=[first, later]), \
                patch.object(interrupt.proc, "process_identity", side_effect=identity), \
                patch.object(interrupt.os, "kill") as kill:
            try:
                interrupt.freeze(owned, {root: (*first[root], "native-root")})
            finally:
                self.assertNotIn(child, owned)
                self.assertTrue(all(call.args[0] == root for call in kill.call_args_list))
        self.assertEqual(owned, {root: (*first[root], "native-root")})

    def test_exited_child_does_not_abort_capture(self):
        """ESRCH means the child is gone; the next census still proves closure."""
        self.capture(ProcessLookupError(3, "cannot read process incarnation"))

    def test_denied_identity_remains_an_error(self):
        """An unreadable live identity cannot authorize successful cleanup."""
        with self.assertRaises(PermissionError):
            self.capture(PermissionError(1, "identity access denied"))



def census_unavailable(*_args):
    """Stand in for a process census that timed out under machine load."""
    raise subprocess.TimeoutExpired(["ps", "-axo", "pid=,ppid=,stat=,lstart="], 5)


class NativeLivenessTests(unittest.TestCase):
    """A captured process is judged by its kernel identity, never by a census (SH-766)."""

    def spawn(self):
        """Start a real child that waits on stdin and record its captured identity."""
        child = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"],
                                 stdin=subprocess.PIPE)
        self.addCleanup(self.reap, child)
        start = interrupt.proc.process_identity(child.pid)["start"]
        return child, {child.pid: (os.getpid(), "census-start", start)}

    def reap(self, child):
        """Continue, end and reap a fixture child whatever state a case left it in."""
        for sig in (signal.SIGCONT, signal.SIGKILL):
            try:
                os.kill(child.pid, sig)
            except ProcessLookupError:
                pass
        child.wait()

    def test_target_binds_a_live_pane_without_a_census(self):
        """Binding reads the pane process natively, so a slow ps cannot refuse it."""
        child, _ = self.spawn()
        answers = {"#{pane_pid}": str(child.pid), "@storyhook-agent": "codex",
                   "#{socket_path}": "/tmp/fixture.sock"}

        def tmux(*args):
            self.assertEqual(args[0], "tmux", "only tmux probes may run while binding")
            return answers[args[-1]]

        with patch.object(interrupt.proc, "run", side_effect=tmux), \
                patch.object(interrupt.proc, "processes", side_effect=census_unavailable):
            bound = interrupt.target("%1", "codex")
        self.assertEqual(bound.split(",")[2], str(child.pid))
        self.assertIn(interrupt.proc.process_identity(child.pid)["start"], bound)

    def test_resuming_a_frozen_process_needs_no_census(self):
        """A failed census must not leave a captured process stopped."""
        child, owned = self.spawn()
        os.kill(child.pid, signal.SIGSTOP)
        _, status = os.waitpid(child.pid, os.WUNTRACED)
        self.assertTrue(os.WIFSTOPPED(status))
        with patch.object(interrupt.proc, "processes", side_effect=census_unavailable):
            interrupt.proc.signal_known(owned, signal.SIGCONT)
        _, status = os.waitpid(child.pid, os.WCONTINUED | os.WNOHANG)
        self.assertTrue(os.WIFCONTINUED(status), "the frozen child was not continued")

    def test_a_replaced_incarnation_is_never_signalled(self):
        """A PID whose native start differs from the capture is another process."""
        child, owned = self.spawn()
        stale = {pid: (*identity[:2], "macos:0:0") for pid, identity in owned.items()}
        with patch.object(interrupt.os, "kill") as kill:
            interrupt.proc.signal_known(stale, signal.SIGSTOP)
        kill.assert_not_called()
        self.assertTrue(interrupt.proc.alive(child.pid, owned[child.pid]))

    def test_an_exited_unreaped_child_reads_as_gone(self):
        """A zombie is dead: exit, not a probe failure, even before its parent reaps it."""
        child, owned = self.spawn()
        child.stdin.close()
        os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOWAIT)
        with self.assertRaises(ProcessLookupError):
            interrupt.proc.process_identity(child.pid)
        self.assertFalse(interrupt.proc.alive(child.pid, owned[child.pid]))


if __name__ == "__main__":
    unittest.main()
