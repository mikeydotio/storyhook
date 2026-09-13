"""Refuse invalid alias staging before helpers can change existing resources."""

import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from development_artifact import prepare
from packaging_evidence import inventory
from stage_alias import stage_alias


class AliasRefusalTests(unittest.TestCase):
    """Malformed identity and existing resources never reach registration helpers."""

    def setUp(self):
        """Create only private inert archive bytes and an existing marketplace."""
        self.temp = tempfile.TemporaryDirectory(prefix="sh707-alias-refusal-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.artifact = self.root / "artifact"
        self.artifact.mkdir()
        data = b"refusal must happen before this inert archive is opened"
        (self.artifact / "greenlight-sh707-development.tar.gz").write_bytes(data)
        self.digest = hashlib.sha256(data).hexdigest()
        self.name = "greenlight-sh707-age54"
        (self.artifact / "provenance.json").write_text(json.dumps({
            "plugin_name": self.name, "artifact_sha256": self.digest}))
        self.home = self.root / "home"
        self.market = self.home / ".agents/plugins/marketplace.json"
        self.market.parent.mkdir(parents=True)
        self.market.write_text(json.dumps({"name": "personal", "plugins": [
            {"name": "existing", "source": {"source": "local", "path": "./plugins/existing"}}]}))
        existing = self.home / "plugins/existing/sentinel"
        existing.parent.mkdir(parents=True)
        existing.write_text("preserve these existing bytes")

    def assert_refusal(self, reason, home=None, digest=None):
        """Check that the actual preflight refuses without helper calls or writes."""
        before = inventory(self.home)
        with mock.patch("stage_alias.subprocess.run", side_effect=AssertionError("helper reached")):
            with self.assertRaisesRegex(ValueError, reason):
                stage_alias(self.artifact, home or self.home, digest or self.digest)
        self.assertEqual(inventory(self.home), before)

    def test_bad_archive_identity_preserves_existing_resources(self):
        """The expected checksum gates even a syntactically valid registration."""
        self.assert_refusal("archive identity", digest="0" * 64)

    def test_existing_source_is_never_overwritten(self):
        """A preexisting alias directory is preserved, including untracked content."""
        target = self.home / "plugins" / self.name
        target.mkdir()
        (target / "operator.txt").write_text("operator work")
        self.assert_refusal("source path already exists")

    def test_existing_registration_is_never_retargeted(self):
        """A registered name cannot be reused even when its source is absent."""
        current = json.loads(self.market.read_text())
        current["plugins"].append({"name": self.name, "source": {"source": "local", "path": "./elsewhere"}})
        self.market.write_text(json.dumps(current))
        self.assert_refusal("already registered")

    def test_symlinked_home_cannot_redirect_staging(self):
        """Filesystem aliases cannot redirect writes past the checked home."""
        alias = self.root / "home-alias"
        alias.symlink_to(self.home, target_is_directory=True)
        self.assert_refusal("symlinked staging path", home=alias)

    def test_unsafe_name_fails_before_output_creation(self):
        """Generator names cannot escape the new artifact output directory."""
        output = self.root / "never-created"
        with self.assertRaisesRegex(ValueError, "plugin name"):
            prepare(self.root, output, plugin_name="../escape")
        self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
