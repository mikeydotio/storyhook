"""Negative controls for installation receipts; no user configuration is touched."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from packaging_evidence import export_source, inventory, validate_install


class IdentityTests(unittest.TestCase):
    """An installer success alone must never certify incomplete or wrong bytes."""

    def setUp(self):
        """Create a small plugin and a real Git origin for immutable export tests."""
        self.scratch = tempfile.TemporaryDirectory(prefix="sh707-identity-", dir="/tmp")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.plugin = self.repo / "plugins/greenlight"
        (self.plugin / ".claude-plugin").mkdir(parents=True)
        (self.plugin / ".claude-plugin/plugin.json").write_text(
            json.dumps({"name": "greenlight", "version": "3.9.1"}))
        self.hook = self.plugin / "hook.sh"
        self.hook.write_text("#!/bin/sh\nexit 0\n")
        self.hook.chmod(0o755)
        self.git("init", "-q")
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "commit.gpgsign=false", "commit", "-qm", "fixture")
        self.commit = self.git("rev-parse", "HEAD").strip()

    def git(self, *args):
        """Run real Git with no inherited repository selectors or user config."""
        return subprocess.check_output(
            ["git", "-C", str(self.repo), *args], text=True,
            env={"PATH": os.environ["PATH"], "HOME": str(self.root), "TMPDIR": "/tmp",
                 "GIT_CONFIG_NOSYSTEM": "1", "LC_ALL": "C"})

    def prepared(self):
        """Return matching source, installer output and minimal configuration."""
        installed = self.root / "market/plugins/greenlight"
        source = export_source(self.repo, self.commit, installed)
        outcome = {"pluginId": "greenlight@sh707-install", "name": "greenlight",
                   "marketplaceName": "sh707-install", "version": "3.9.1",
                   "installedPath": str(installed)}
        config = {"marketplaces": {"sh707-install": {
            "source_type": "local", "source": str(self.root / "market")}},
            "plugins": {"greenlight@sh707-install": {"enabled": True}}}
        source["marketplace_root"] = str(self.root / "market")
        return source, installed, outcome, config

    def test_committed_export_ignores_concurrent_working_edits(self):
        """Export the pinned commit even if another owner edits the worktree."""
        self.hook.write_text("changed\n")
        source = export_source(self.repo, self.commit, self.root / "export")
        self.assertEqual((self.root / "export/hook.sh").read_text(), "#!/bin/sh\nexit 0\n")
        self.assertEqual(source["commit"], self.commit)
        self.assertEqual(source["files"], inventory(self.root / "export"))
        self.assertEqual(source["files"]["hook.sh"]["mode"], "100755")

    def test_export_refuses_moving_refs_and_occupied_destination(self):
        """A symbolic ref or existing destination is not an immutable export."""
        for commit, destination in (("HEAD", self.root / "export"),
                                    (self.commit, self.plugin)):
            with self.subTest(commit=commit, destination=destination):
                with self.assertRaises(ValueError):
                    export_source(self.repo, commit, destination)

    def test_local_git_replacement_cannot_change_pinned_content(self):
        """Local replace refs must not redefine bytes identified by a commit."""
        original = self.git("rev-parse", "HEAD:plugins/greenlight/hook.sh").strip()
        replacement = self.root / "replacement"
        replacement.write_text("not the committed hook\n")
        oid = self.git("hash-object", "-w", str(replacement)).strip()
        self.git("replace", original, oid)
        destination = self.root / "export"
        export_source(self.repo, self.commit, destination)
        self.assertEqual((destination / "hook.sh").read_text(), "#!/bin/sh\nexit 0\n")

    def test_matching_installation_has_candidate_only_receipt(self):
        """Packaging parity makes no claim about release or active host trust."""
        receipt = validate_install(*self.prepared())
        self.assertEqual(receipt["status"], "packaging-parity")
        self.assertFalse(receipt["release_certified"])
        self.assertFalse(receipt["active_host_validated"])

    def test_export_refuses_committed_symlink_before_writing(self):
        """Git-stored links cannot escape the export or reach the installer."""
        (self.plugin / "linked").symlink_to("../../outside")
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "commit.gpgsign=false", "commit", "-qm", "unsafe fixture")
        destination = self.root / "export"
        with self.assertRaisesRegex(ValueError, "unsafe"):
            export_source(self.repo, self.git("rev-parse", "HEAD").strip(), destination)
        self.assertFalse(destination.exists())

    def test_missing_or_symlinked_install_root_fails(self):
        """Do not certify an absent cache or resolve a substituted root silently."""
        source, installed, outcome, config = self.prepared()
        moved = installed.with_name("real-plugin")
        installed.rename(moved)
        with self.assertRaisesRegex(ValueError, "unsafe or missing"):
            validate_install(source, installed, outcome, config)
        installed.symlink_to(moved, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "unsafe or missing"):
            validate_install(source, installed, outcome, config)

    def test_altered_missing_extra_and_nonexecutable_files_fail(self):
        """Content, completeness and executable modes all belong to identity."""
        source, installed, outcome, config = self.prepared()
        path = installed / "hook.sh"
        original = path.read_bytes()
        for mutation in ("bytes", "missing", "extra", "mode"):
            with self.subTest(mutation=mutation):
                path.write_bytes(original)
                path.chmod(0o755)
                extra = installed / "extra"
                if extra.exists():
                    extra.unlink()
                if mutation == "bytes":
                    path.write_text("wrong")
                elif mutation == "missing":
                    path.unlink()
                elif mutation == "extra":
                    extra.write_text("unexpected")
                else:
                    path.chmod(0o644)
                with self.assertRaisesRegex(ValueError, "parity"):
                    validate_install(source, installed, outcome, config)

    def test_wrong_installer_identity_fails(self):
        """Do not accept another plugin, version, marketplace or cache path."""
        source, installed, outcome, config = self.prepared()
        for key in ("pluginId", "name", "marketplaceName", "version", "installedPath"):
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "identity"):
                validate_install(source, installed, {**outcome, key: "wrong"}, config)

    def test_disabled_or_wrongly_registered_plugin_fails(self):
        """Matching bytes cannot compensate for registration or enablement loss."""
        source, installed, outcome, config = self.prepared()
        config["plugins"]["greenlight@sh707-install"]["enabled"] = False
        with self.assertRaisesRegex(ValueError, "enabled"):
            validate_install(source, installed, outcome, config)
        config["plugins"]["greenlight@sh707-install"]["enabled"] = True
        config["marketplaces"]["sh707-install"]["source"] = "/wrong"
        with self.assertRaisesRegex(ValueError, "registration"):
            validate_install(source, installed, outcome, config)

    def test_symlinks_and_special_files_fail(self):
        """Even an internal link is outside the pinned regular-file contract."""
        for kind in ("symlink", "fifo"):
            with self.subTest(kind=kind):
                path = self.plugin / "unsafe"
                if kind == "symlink":
                    path.symlink_to("hook.sh")
                else:
                    os.mkfifo(path)
                try:
                    with self.assertRaisesRegex(ValueError, "unsafe"):
                        inventory(self.plugin)
                finally:
                    path.unlink()


if __name__ == "__main__":
    unittest.main(verbosity=2)
