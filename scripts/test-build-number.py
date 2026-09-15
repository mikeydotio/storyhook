"""Exercise the production build-number wrapper in isolated source directories."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

WRAPPER = Path(__file__).with_name("build-number.py").resolve()


class BuildNumberTests(unittest.TestCase):
    """Number allocation must survive failures and concurrent publishers."""

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="SH-732-", dir="/tmp")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        (self.root / "BUILD").write_text("0\n")

    def command(self, *args):
        return [sys.executable, str(WRAPPER), "--root", str(self.root), *args]

    def run_wrapper(self, *args):
        return subprocess.run(self.command(*args), capture_output=True, text=True)

    def test_sequential_and_failure_consume_numbers(self):
        for expected, exit_code in [(1, 0), (2, 17), (3, 0)]:
            result = self.run_wrapper("--", sys.executable, "-c", f"raise SystemExit({exit_code})")
            self.assertEqual(result.returncode, exit_code, result.stderr)
            self.assertEqual((self.root / "BUILD").read_text(), f"{expected}\n")

    def test_validation_fails_without_overwriting(self):
        for bad in ["", "-1\n", "01\n", "1", "1\n2\n", "1 \n", "18446744073709551616\n"]:
            (self.root / "BUILD").write_text(bad)
            result = self.run_wrapper("--reserve")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(str(self.root / "BUILD"), result.stderr)
            self.assertEqual((self.root / "BUILD").read_text(), bad)
        (self.root / "BUILD").unlink()
        self.assertNotEqual(self.run_wrapper("--reserve").returncode, 0)

    def test_overflow_and_symlink_refused(self):
        (self.root / "BUILD").write_text("18446744073709551615\n")
        self.assertNotEqual(self.run_wrapper("--reserve").returncode, 0)
        (self.root / "BUILD").rename(self.root / "other")
        (self.root / "BUILD").symlink_to(self.root / "other")
        self.assertNotEqual(self.run_wrapper("--reserve").returncode, 0)
        self.assertEqual((self.root / "other").read_text(), "18446744073709551615\n")

    def test_readonly_counter_refused_without_changing_state(self):
        (self.root / "BUILD").chmod(0o444)
        try:
            result = self.run_wrapper("--reserve")
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual((self.root / "BUILD").read_text(), "0\n")
        finally:
            (self.root / "BUILD").chmod(0o644)

    def test_internal_entry_requires_inherited_ownership(self):
        self.assertNotEqual(self.run_wrapper("--check-lock").returncode, 0)
        result = self.run_wrapper("--", *self.command("--check-lock"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "1")

    def test_reserved_build_is_reused_only_when_current(self):
        result = self.run_wrapper("--reserve")
        self.assertEqual(result.stdout.strip(), "1", result.stderr)
        self.assertEqual(self.run_wrapper("--number", "1", "--", "true").returncode, 0)
        self.assertEqual((self.root / "BUILD").read_text(), "1\n")
        self.assertNotEqual(self.run_wrapper("--number", "0", "--", "true").returncode, 0)

    def test_check_does_not_allocate(self):
        result = self.run_wrapper("--check")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "0")
        self.assertEqual((self.root / "BUILD").read_text(), "0\n")

    def test_make_install_allocates_once_and_dry_run_does_not_allocate(self):
        import shutil
        (self.root / "scripts").mkdir()
        shutil.copyfile(WRAPPER, self.root / "scripts/build-number.py")
        shutil.copyfile(WRAPPER.parent.parent / "Makefile", self.root / "Makefile")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        cargo = bin_dir / "cargo"
        cargo.write_text("""#!/bin/sh
set -eu
mkdir -p target/release
number=$(cat BUILD)
printf '#!/bin/sh\\nif [ "$1" = --version ]; then echo "story 3.0.0 (%s)"; fi\\n' "$number" > target/release/story
chmod +x target/release/story
""")
        cargo.chmod(0o755)
        environment = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ["PATH"])
        environment.pop("MAKEFLAGS", None)
        destination = self.root / "installed"
        def make(*arguments):
            return subprocess.run(["make", *arguments, f"INSTALL_DIR={destination}"],
                                  cwd=self.root, env=environment, text=True, capture_output=True)
        self.assertEqual(make("-n", "install").returncode, 0)
        self.assertEqual((self.root / "BUILD").read_text(), "0\n")
        self.assertNotEqual(make("_install-build").returncode, 0)
        self.assertFalse(destination.exists())
        for number in (1, 2):
            result = make("install")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((self.root / "BUILD").read_text(), f"{number}\n")
            installed = subprocess.check_output([destination / "story", "--version"], text=True)
            self.assertEqual(installed.strip(), f"story 3.0.0 ({number})")
            self.assertIn(installed.strip(), result.stdout)
        self.assertEqual(make("release-build").returncode, 0)
        self.assertEqual((self.root / "BUILD").read_text(), "3\n")

    def test_concurrent_commands_keep_their_number_through_collection(self):
        child = """from pathlib import Path
import time
n = Path('BUILD').read_text()
time.sleep(.05)
assert Path('BUILD').read_text() == n
Path('artifact-' + n.strip()).write_text(n)
"""
        children = [subprocess.Popen(self.command("--", sys.executable, "-c", child),
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(8)]
        for process in children:
            _, err = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, err)
        self.assertEqual((self.root / "BUILD").read_text(), "8\n")
        self.assertEqual(len(list(self.root.glob("artifact-*"))), 8)

    def test_killed_wrapper_leaves_child_holding_lock(self):
        child = """from pathlib import Path
import time
Path('ready').touch()
while not Path('finish').exists(): time.sleep(.01)
"""
        first = subprocess.Popen(self.command("--", sys.executable, "-c", child))
        try:
            deadline = time.monotonic() + 5
            while not (self.root / "ready").exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue((self.root / "ready").exists())
            first.kill()
            first.wait(timeout=5)
            second = subprocess.Popen(self.command("--reserve"), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            time.sleep(.1)
            self.assertIsNone(second.poll())
            (self.root / "finish").touch()
            out, err = second.communicate(timeout=5)
            self.assertEqual(second.returncode, 0, err)
            self.assertEqual(out.strip(), b"2")
        finally:
            (self.root / "finish").touch()
            if first.poll() is None:
                first.kill()
            first.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
