"""Fixture evidence must never authorize removal of a production lookalike."""

import importlib.util
from pathlib import Path
import shlex
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("cleanup", Path(__file__).resolve().parents[1] / "cleanup-verifier-fixtures.py")
CLEANUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CLEANUP)


class CleanupTests(unittest.TestCase):
    """Compare exact immutable evidence, including race and multi-pane cases."""

    def fixture(self):
        return ["storyhook-verifier", "@100", "activity-data-" + "a" * 64, "%100", "123",
                shlex.join(["/bin/story", "--store-path", "/private/tmp/agentics-store.abc/data/store.db", "daemon", "logs", "--follow"])]

    def test_proven_activity_and_banner_only(self):
        row = self.fixture()
        self.assertIsNotNone(CLEANUP.classify(row))
        for index, replacement in [(0, "other"), (1, "@100;other"), (2, "activity-data-production"),
                                   (5, row[5].replace("/private/tmp/agentics-store.abc", "/Users/me")),
                                   (5, row[5].replace("/private/tmp/agentics-store.abc", "/private/tmp/agentics-store.abc/../production"))]:
            changed = list(row)
            changed[index] = replacement
            self.assertIsNone(CLEANUP.classify(changed))
        row[2] = "verification-storyhook-fixture-abc-" + "b" * 40
        row[5] = shlex.join(["bash", "-c", 'printf "%s\\n" "$1"; exec sleep 2147483647',
                            "verifier-banner", "verifying https://github.com/acme/widgets/pull/7 — metadata"])
        self.assertIsNotNone(CLEANUP.classify(row))
        row[5] = row[5].replace("acme/widgets", "mikeydotio/storyhook")
        self.assertIsNone(CLEANUP.classify(row))

    def test_inventory_is_read_only_and_changed_or_multiple_panes_are_kept(self):
        row = self.fixture()
        with patch.object(CLEANUP, "inventory", return_value=[row]), patch.object(CLEANUP, "tmux") as control:
            self.assertEqual(CLEANUP.cleanup()[0]["action"], "candidate")
            control.assert_not_called()
        changed = list(row)
        changed[3] = "%101"
        for observations in [[[row], [changed]], [[row, changed]]]:
            with patch.object(CLEANUP, "inventory", side_effect=observations), patch.object(CLEANUP, "tmux") as control:
                CLEANUP.cleanup(apply=True)
                control.assert_not_called()

    def test_apply_uses_exact_id_and_checks_removal(self):
        row = self.fixture()
        with patch.object(CLEANUP, "inventory", side_effect=[[row], [row], []]), patch.object(CLEANUP, "tmux") as control:
            self.assertEqual(CLEANUP.cleanup(apply=True)[0]["action"], "removed")
            control.assert_called_once_with("kill-window", "-t", "@100")


if __name__ == "__main__":
    unittest.main()
