"""Pin interruption capture races at the native process-probe boundary."""

import importlib.util
from pathlib import Path
import signal
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


if __name__ == "__main__":
    unittest.main()
