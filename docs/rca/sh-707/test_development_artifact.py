"""Fence the exact metadata-only exception authorized for the SH-707 artifact."""

import json
from pathlib import Path
import tempfile
import unittest

from development_artifact import OVERLAY, validate_overlay
from packaging_evidence import inventory


class OverlayTests(unittest.TestCase):
    """Development labeling cannot hide modified source or a release identity."""

    def setUp(self):
        """Build a tiny original plugin and its explicit Codex overlay."""
        self.temp = tempfile.TemporaryDirectory(prefix="sh707-overlay-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.plugin = Path(self.temp.name)
        original = self.plugin / ".claude-plugin/plugin.json"
        original.parent.mkdir()
        original.write_text(json.dumps({"name": "greenlight", "version": "3.9.1"}))
        self.hook = self.plugin / "hook.sh"
        self.hook.write_text("production bytes\n")
        self.source = {"files": inventory(self.plugin), "version": "3.9.1"}
        self.overlay = self.plugin / OVERLAY
        self.overlay.parent.mkdir()
        self.manifest = {"name": "greenlight", "version": "3.9.1+codex.20260913020702"}
        self.overlay.write_text(json.dumps(self.manifest))

    def test_only_added_overlay_is_accepted(self):
        """Receipt identifies the sole addition and preserved original files."""
        result = validate_overlay(self.source, self.plugin)
        self.assertEqual(result["added"], [OVERLAY])
        self.assertEqual(result["modified"], [])
        self.assertEqual(result["removed"], [])

    def test_changed_originals_are_refused(self):
        """Neither executable content nor the Claude release manifest may drift."""
        for path in (self.hook, self.plugin / ".claude-plugin/plugin.json"):
            old = path.read_bytes()
            with self.subTest(path=path):
                path.write_text("changed")
                try:
                    with self.assertRaisesRegex(ValueError, "source"):
                        validate_overlay(self.source, self.plugin)
                finally:
                    path.write_bytes(old)

    def test_removed_and_extra_files_are_refused(self):
        """The metadata exception is exact; omissions and extra code fail."""
        old = self.hook.read_bytes()
        self.hook.unlink()
        with self.assertRaisesRegex(ValueError, "source"):
            validate_overlay(self.source, self.plugin)
        self.hook.write_bytes(old)
        (self.plugin / "other.py").write_text("unexpected")
        with self.assertRaisesRegex(ValueError, "addition"):
            validate_overlay(self.source, self.plugin)

    def test_missing_overlay_wrong_name_and_release_versions_are_refused(self):
        """A local artifact must carry its own explicit Codex cache identity."""
        for version in ("3.9.1", "local", "3.9.1+codex.", "3.9.1+other.123", "9.0.0+codex.123"):
            with self.subTest(version=version):
                self.overlay.write_text(json.dumps({**self.manifest, "version": version}))
                with self.assertRaisesRegex(ValueError, "identity"):
                    validate_overlay(self.source, self.plugin)
        self.overlay.write_text(json.dumps({**self.manifest, "name": "other"}))
        with self.assertRaisesRegex(ValueError, "identity"):
            validate_overlay(self.source, self.plugin)
        self.overlay.unlink()
        with self.assertRaisesRegex(ValueError, "addition"):
            validate_overlay(self.source, self.plugin)


if __name__ == "__main__":
    unittest.main(verbosity=2)
