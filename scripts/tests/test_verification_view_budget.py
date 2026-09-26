"""The view's operation clock covers every probe and preserves failure evidence."""

from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def program():
    """Load the shipping sources without invoking their command-line entry point."""
    module = types.ModuleType("verification_view")
    source = ((ROOT / "plugins/story/lib/probe_budget.py").read_text()
              + "\nprobe_run = run\nprobe_operation = operation\n"
              + (ROOT / "plugins/story/lib/tmux_server_env.py").read_text()
              + "\n" + (ROOT / "scripts/verification-view.py").read_text())
    exec(compile(source, "verification_view", "exec"), module.__dict__)
    return module


class BudgetTests(unittest.TestCase):
    """Only external process answers and the monotonic clock are controlled."""

    def test_one_deadline_covers_the_entire_reconcile(self):
        view = program()
        clock = [0.0]
        allowances = []

        def answer(argv, **kwargs):
            allowances.append(kwargs["timeout"])
            clock[0] += 1
            return subprocess.CompletedProcess(argv, 0, "@1" if argv[1] == "new-session" else "", "")

        with tempfile.TemporaryDirectory(dir="/tmp") as directory, \
                patch.dict(view.os.environ, STORYHOOK_VERIFIER_MIRROR="1"), \
                patch.object(view.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(view.subprocess, "run", side_effect=answer):
            view.reconcile("project", directory, "/bin/true")
        self.assertGreater(len(allowances), 5)
        self.assertEqual(allowances, [view.BUDGET_SECONDS - n for n in range(len(allowances))])

    def test_exhausted_cleanup_preserves_the_primary_failure_and_starts_no_client(self):
        view = program()
        clock = [0.0]
        calls = []

        def answer(argv, **kwargs):
            calls.append(argv[1])
            if argv[1] == "display-message":
                clock[0] = view.BUDGET_SECONDS
                return subprocess.CompletedProcess(argv, 1, "", "original mark failure")
            return subprocess.CompletedProcess(argv, 0, "@1" if argv[1] == "new-session" else "", "")

        with tempfile.TemporaryDirectory(dir="/tmp") as directory, \
                patch.dict(view.os.environ, STORYHOOK_VERIFIER_MIRROR="1"), \
                patch.object(view.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(view.subprocess, "run", side_effect=answer):
            with self.assertRaises(RuntimeError) as failed:
                view.reconcile("project", directory, "/bin/true")
        self.assertIn("original mark failure", str(failed.exception))
        self.assertIn("cleanup", str(failed.exception))
        self.assertIn("operation budget", str(failed.exception))
        self.assertNotIn("kill-window", calls)

    def test_an_existing_operation_cannot_be_extended_by_reconcile(self):
        view = program()
        with tempfile.TemporaryDirectory(dir="/tmp") as directory, \
                patch.dict(view.os.environ, STORYHOOK_VERIFIER_MIRROR="1"), \
                patch.object(view.subprocess, "run") as run:
            with view.operation(0), self.assertRaises(view.ProbeTimeout):
                view.reconcile("project", directory, "/bin/true")
        run.assert_not_called()
