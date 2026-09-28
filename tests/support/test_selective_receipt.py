"""Keep selective receipt command patience tied to the shared load policy."""
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/tests"))
import load_grace
from selective_receipt import Fixture


class CommandPatience(unittest.TestCase):
    """Exercise the fixture command boundary under controlled contention."""

    def test_command_allowance_tracks_load_and_keeps_the_shared_ceiling(self):
        """Each command resamples load; idle and unknown load retain 30 seconds."""
        fixture = Fixture.__new__(Fixture)
        fixture.root = Path("/tmp")
        fixture.env = {}
        completed = subprocess.CompletedProcess(["fixture"], 0, "", "")
        with patch.object(load_grace, "contention", side_effect=[None, 1, 4, 100]), \
                patch("selective_receipt.subprocess.run", return_value=completed) as run:
            for expected in (30, 30, 120, load_grace.PATIENCE_CEILING):
                fixture.run(["fixture"])
                self.assertEqual(run.call_args.kwargs["timeout"], expected)

    def test_timeout_remains_an_error(self):
        """Grace must not hide a command that never completes."""
        fixture = Fixture.__new__(Fixture)
        fixture.root = Path("/tmp")
        fixture.env = {}
        with patch("selective_receipt.subprocess.run", side_effect=subprocess.TimeoutExpired("fixture", 30)):
            with self.assertRaises(subprocess.TimeoutExpired):
                fixture.run(["fixture"])


if __name__ == "__main__":
    unittest.main()
