"""Prove staging refuses unsafe inputs before creating personal registration."""

import hashlib
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from development_artifact import validate_overlay
from packaging_evidence import inventory
from stage_personal import stage


class StageRefusalTests(unittest.TestCase):
    """No preflight failure may call the official destination-writing helper."""

    def setUp(self):
        """Seal a minimal real archive and independently editable sidecar."""
        self.temp = tempfile.TemporaryDirectory(prefix="sh707-preflight-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name).resolve()
        self.artifacts = root / "artifacts"
        self.artifacts.mkdir()
        self.home = root / "target"
        self.home.mkdir()
        payload = root / "payload"
        plugin = payload / "plugins/greenlight"
        (plugin / ".claude-plugin").mkdir(parents=True)
        manifest = {"name": "greenlight", "version": "3.9.1"}
        (plugin / ".claude-plugin/plugin.json").write_text(json.dumps(manifest))
        source = {"files": inventory(plugin), "version": "3.9.1"}
        (plugin / ".codex-plugin").mkdir()
        (plugin / ".codex-plugin/plugin.json").write_text(json.dumps(
            {**manifest, "version": "3.9.1+codex.20260913021137"}))
        marketplace = payload / ".agents/plugins/marketplace.json"
        marketplace.parent.mkdir(parents=True)
        marketplace.write_text("{}")
        archive = self.artifacts / "greenlight-sh707-development.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle:
            for path in sorted(payload.rglob("*")):
                bundle.add(path, arcname=path.relative_to(payload), recursive=False)
        self.digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.receipt = {"artifact_sha256": self.digest, "source": source,
                        "payload_files": inventory(payload),
                        "metadata_delta": validate_overlay(source, plugin)}
        self.receipt_path = self.artifacts / "provenance.json"
        self.receipt_path.write_text(json.dumps(self.receipt))

    def refused(self, message, digest=None):
        """Assert rejection leaves the entire target home unchanged."""
        before = inventory(self.home)
        with patch("stage_personal.subprocess.run") as helper:
            with self.assertRaisesRegex(ValueError, message):
                stage(self.artifacts, self.home, digest or self.digest)
            helper.assert_not_called()
        self.assertEqual(inventory(self.home), before)

    def test_wrong_reviewed_archive_hash_is_refused(self):
        """An otherwise consistent artifact cannot substitute for the reviewed one."""
        self.refused("archive identity", "0" * 64)

    def test_existing_marketplace_and_source_are_refused(self):
        """No preexisting personal entry or source directory can be overwritten."""
        for relative in (".agents/plugins/marketplace.json", "plugins/greenlight/witness"):
            with self.subTest(relative=relative):
                target = self.home / relative
                target.parent.mkdir(parents=True)
                target.write_text("preserve")
                self.refused("existing destination")
                target.unlink()

    def test_symlinked_ancestor_is_refused(self):
        """Destination resolution cannot escape the requested home through a link."""
        outside = self.home.parent / "outside"
        outside.mkdir()
        (self.home / "plugins").symlink_to(outside, target_is_directory=True)
        with patch("stage_personal.subprocess.run") as helper:
            with self.assertRaisesRegex(ValueError, "symlinked destination"):
                stage(self.artifacts, self.home, self.digest)
            helper.assert_not_called()
        self.assertEqual(list(outside.iterdir()), [])

    def test_altered_payload_receipt_is_refused(self):
        """The complete payload inventory must match the sealed bytes."""
        self.receipt["payload_files"].pop(".agents/plugins/marketplace.json")
        self.receipt_path.write_text(json.dumps(self.receipt))
        self.refused("payload inventory")

    def test_altered_copy_receipt_is_refused(self):
        """A sidecar cannot redefine the copy set or destination modes."""
        self.receipt["metadata_delta"]["files"]["../../escape"] = {
            "mode": "100755", "sha256": "0" * 64}
        self.receipt_path.write_text(json.dumps(self.receipt))
        self.refused("metadata receipt")


if __name__ == "__main__":
    unittest.main(verbosity=2)
