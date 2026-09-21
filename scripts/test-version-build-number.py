"""Exercise version build reservations through the production semver hook."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
HOOK = ROOT / ".semver/hooks/pre-bump/sync-build-number.sh"


class VersionBuildNumberTests(unittest.TestCase):
    """Version changes must commit a new build identity without losing edits."""

    def setUp(self):
        """Own all Git and counter state in a disposable checkout."""
        self.scratch = tempfile.TemporaryDirectory(prefix="SH-749-", dir="/tmp")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.environment = {key: value for key, value in os.environ.items()
                            if not key.startswith("GIT_")}
        self.environment.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "-q", "-b", "dev")
        self.git("config", "user.name", "Version Fixture")
        self.git("config", "user.email", "version@example.test")
        self.git("config", "core.hooksPath", os.devnull)
        (self.root / "scripts").mkdir()
        shutil.copyfile(ROOT / "scripts/build-number.py", self.root / "scripts/build-number.py")
        (self.root / "BUILD").write_text("102\n")
        (self.root / "VERSION").write_text("v3.0.2\n")
        (self.root / "notes").write_text("original\n")
        (self.root / ".gitignore").write_text("/.build-number.lock\n/.BUILD-*\n")
        self.git("add", "-A")
        self.git("commit", "-qm", "fixture")

    def git(self, *arguments):
        """Run real Git against this fixture only."""
        result = subprocess.run(["git", *arguments], cwd=self.root,
                                env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def hook(self, old="v3.0.2", new="v3.0.3", kind="patch"):
        """Invoke the production hook with the documented semver environment."""
        environment = dict(self.environment, BUMP_TYPE=kind)
        for key, value in (("OLD_VERSION", old), ("NEW_VERSION", new)):
            environment.pop(key, None)
            if value is not None:
                environment[key] = value
        return subprocess.run(["bash", str(HOOK)], cwd=self.root, env=environment,
                              capture_output=True, text=True)

    def test_each_version_change_reserves_and_stages_exactly_once(self):
        """Bump levels, explicit assignment, and initial versions share the rule."""
        cases = [("patch", "v3.0.2", "v3.0.3"), ("minor", "v3.0.2", "v3.1.0"),
                 ("major", "v3.0.2", "v4.0.0"), ("set", "v3.0.2", "v5.2.1"),
                 ("set", "v3.0.2", "v2.0.0"), ("init", "(none)", "v0.1.0")]
        for kind, old, new in cases:
            with self.subTest(kind=kind, old=old, new=new):
                (self.root / "BUILD").write_text("102\n")
                self.git("add", "BUILD")
                result = self.hook(old, new, kind)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.root / "BUILD").read_text(), "103\n")
                self.assertEqual(self.git("show", ":BUILD"), "103\n")
                self.assertEqual(self.git("diff", "--cached", "--name-only"), "BUILD\n")
                self.assertEqual((self.root / "VERSION").read_text(), "v3.0.2\n")

    def test_local_install_advancement_is_incremented_and_committed_with_version(self):
        """A previously allocated installation number is not reused for a version."""
        (self.root / "BUILD").write_text("103\n")
        (self.root / "notes").write_text("preserve this edit\n")
        result = self.hook()
        self.assertEqual(result.returncode, 0, result.stderr)
        (self.root / "VERSION").write_text("v3.0.3\n")
        self.git("add", "VERSION")
        self.git("commit", "-qm", "version change")
        self.assertEqual(self.git("show", "HEAD:BUILD"), "104\n")
        self.assertEqual(self.git("show", "HEAD:VERSION"), "v3.0.3\n")
        self.assertEqual(self.git("status", "--porcelain"), " M notes\n")

    def test_same_version_does_not_allocate_or_stage(self):
        """Recutting metadata without a VERSION change retains its build."""
        (self.root / "BUILD").write_text("103\n")
        result = self.hook(new="v3.0.2", kind="set")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "BUILD").read_text(), "103\n")
        self.assertEqual(self.git("diff", "--cached", "--name-only"), "")
        self.assertFalse((self.root / ".build-number.lock").exists())

    def test_missing_or_empty_context_fails_before_allocating(self):
        """A malformed hook invocation cannot silently consume a number."""
        for old, new in [(None, "v3.0.3"), ("", "v3.0.3"),
                         ("v3.0.2", None), ("v3.0.2", ""), (None, None)]:
            with self.subTest(old=old, new=new):
                result = self.hook(old, new)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("VERSION", result.stderr)
                self.assertEqual((self.root / "BUILD").read_text(), "102\n")
                self.assertEqual(self.git("diff", "--cached", "--name-only"), "")

    def test_counter_failures_abort_without_replacing_state(self):
        """Allocator validation and overflow errors propagate through the hook."""
        for value in ["", "01\n", "-1\n", "1", "18446744073709551615\n",
                      "18446744073709551616\n"]:
            with self.subTest(value=value):
                (self.root / "BUILD").write_text(value)
                result = self.hook()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("BUILD", result.stderr)
                self.assertEqual((self.root / "BUILD").read_text(), value)
                self.assertEqual(self.git("show", ":BUILD"), "102\n")
        (self.root / "BUILD").unlink()
        self.assertNotEqual(self.hook().returncode, 0)
        self.assertFalse((self.root / "BUILD").exists())

    def test_readonly_counter_aborts(self):
        """An unwritable counter must not allow the release to continue."""
        (self.root / "BUILD").chmod(0o444)
        try:
            result = self.hook()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("BUILD", result.stderr)
            self.assertEqual((self.root / "BUILD").read_text(), "102\n")
        finally:
            (self.root / "BUILD").chmod(0o644)

    def test_staging_failure_is_loud_and_consumes_the_reservation(self):
        """A failed Git write preserves the allocator's no-reuse guarantee."""
        lock = self.root / ".git/index.lock"
        lock.touch()
        result = self.hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("index.lock", result.stderr)
        self.assertEqual((self.root / "BUILD").read_text(), "103\n")
        self.assertEqual((self.root / "VERSION").read_text(), "v3.0.2\n")
        lock.unlink()
        result = self.hook()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("show", ":BUILD"), "104\n")

    def test_concurrent_reservations_serialize_through_staging(self):
        """The last staged number must equal the final durable counter."""
        environment = dict(self.environment, OLD_VERSION="v3.0.2", NEW_VERSION="v3.0.3")
        children = [subprocess.Popen(["bash", str(HOOK)], cwd=self.root, env=environment,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    for _ in range(6)]
        try:
            for child in children:
                _, error = child.communicate(timeout=10)
                self.assertEqual(child.returncode, 0, error)
        finally:
            for child in children:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=5)
        self.assertEqual((self.root / "BUILD").read_text(), "108\n")
        self.assertEqual(self.git("show", ":BUILD"), "108\n")

    def test_hook_is_executable_for_semver_discovery(self):
        """The real runner ignores hooks that lack the executable bit."""
        self.assertTrue(os.access(HOOK, os.X_OK))


if __name__ == "__main__":
    unittest.main()
