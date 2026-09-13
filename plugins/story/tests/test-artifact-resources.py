"""Component-aware installed artifact protection, independent of live resources."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "artifact_resources", Path(__file__).resolve().parents[1] / "lib/artifact-resources.py"
)
GUARD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GUARD)


class ResourceProtection(unittest.TestCase):
    """Exercise real filesystem identities and malformed protection evidence."""

    def setUp(self):
        """Own all fixture resources under an unindexed scratch directory."""
        self.temporary = tempfile.TemporaryDirectory(prefix="sh708-", dir="/tmp")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.managed = self.root / "installed"
        self.managed.mkdir()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.manifest = self.root / "managed-paths"
        self.manifest.write_text(f"# installer fixture\n{self.managed}\n")

    def check(self, writes=None, removal=None):
        """Invoke the production checker against fixture paths."""
        GUARD.check_resources(self.manifest, writes or [self.repo], removal or self.repo / "worktree")

    def test_normal_and_missing_manifest(self):
        """An ordinary checkout and an uninstalled host remain usable."""
        self.check()
        self.manifest.unlink()
        self.check()

    def test_no_worktree_still_protects_write_locations(self):
        """Branch-only operations have no removal target but still write Git."""
        GUARD.check_resources(self.manifest, [self.repo], None)
        with self.assertRaisesRegex(ValueError, "installed"):
            GUARD.check_resources(self.manifest, [self.managed], None)

    def test_component_boundaries(self):
        """A shared string prefix is not a path containment relationship."""
        self.check(writes=[self.root / "installed-other"], removal=self.root / "installed-other/wt")

    def test_managed_write_locations(self):
        """Protect checkout and common Git directories within installations."""
        for target in [self.managed, self.managed / "repo", self.managed / "git"]:
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, "installed"):
                self.check(writes=[target])

    def test_removal_overlap_in_both_directions(self):
        """Removing a managed path or its ancestor has the same consequence."""
        for target in [self.managed, self.managed / "wt", self.root]:
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, "installed"):
                self.check(removal=target)

    def test_traversal_and_symlinked_parents(self):
        """Compare spelled and resolved identities without losing traversal."""
        alias = self.repo / "alias"
        alias.symlink_to(self.managed, target_is_directory=True)
        for target in [self.repo / "../installed/wt", alias / "wt"]:
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, "installed"):
                self.check(removal=target)

    def test_symlink_inside_managed_path_cannot_escape_protection(self):
        """Lexical containment still protects a redirected installed entry."""
        alias = self.managed / "alias"
        alias.symlink_to(self.repo, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "installed"):
            self.check(removal=alias / "wt")

    def test_redirected_manifest_prefix(self):
        """A manifest alias protects its actual installed target as well."""
        alias = self.root / "alias"
        alias.symlink_to(self.managed, target_is_directory=True)
        self.manifest.write_text(f"{alias}\n")
        with self.assertRaisesRegex(ValueError, "installed"):
            self.check(removal=self.managed / "wt")

    def test_bad_manifests_fail_loud(self):
        """An existing but invalid registry cannot mean no installation."""
        for contents in ["relative/path\n", "\x00bad\n", "", "# only comments\n"]:
            self.manifest.write_text(contents)
            with self.subTest(contents=contents), self.assertRaises(ValueError):
                self.check()
        self.manifest.write_bytes(b"\xff")
        with self.assertRaises(ValueError):
            self.check()

    def test_special_or_dangling_manifest_fails(self):
        """Never block on a FIFO or mistake a broken registry link for absence."""
        self.manifest.unlink()
        os.mkfifo(self.manifest)
        with self.assertRaises(ValueError):
            self.check()
        self.manifest.unlink()
        self.manifest.symlink_to(self.root / "missing")
        with self.assertRaises(ValueError):
            self.check()

    def test_unreadable_manifest_fails(self):
        """An inaccessible registry is not an uninstalled host."""
        if os.geteuid() == 0:
            self.skipTest("root bypasses file read permissions")
        self.manifest.chmod(0)
        try:
            with self.assertRaises(ValueError):
                self.check()
        finally:
            self.manifest.chmod(0o600)

    def test_unresolved_target_fails(self):
        """Loops and non-directory ancestors are explicit invalid evidence."""
        loop = self.repo / "loop"
        loop.symlink_to(loop)
        file = self.repo / "file"
        file.write_text("sentinel")
        for target in [loop / "wt", file / "wt", Path("relative")]:
            with self.subTest(target=target), self.assertRaises(ValueError):
                self.check(removal=target)


if __name__ == "__main__":
    unittest.main()
