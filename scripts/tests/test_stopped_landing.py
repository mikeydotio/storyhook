"""SH-882: real Git preparation does not invent certification."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class StoppedLanding(unittest.TestCase):
    """Exercise the production merge computation and private landing boundary."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sh882-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.git("init", "-q", "--template=", "-b", "main")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        (self.repo / "base").write_text("base\n")
        self.git("add", ".")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        (self.repo / "new").write_text("head\n")
        self.git("add", ".")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "head")
        self.head = self.git("rev-parse", "HEAD").stdout.strip()
        self.tree = self.git("rev-parse", "HEAD^{tree}").stdout.strip()

    def run_command(self, *args, extra=None):
        """Use the same isolation contract as the repository's shell harnesses."""
        environment = os.environ.copy()
        if extra:
            environment.update(extra)
        return subprocess.run(
            ["bash", "-c", 'source "$1/scripts/test-env.sh"; storyhook_isolate --home "$2"; shift 2; exec "$@"',
             "sh882", str(ROOT), str(self.root / "environment"), *map(str, args)],
            cwd=self.repo, env=environment, capture_output=True, text=True, check=False,
        )

    def git(self, *args):
        """Run real local Git and retain a complete diagnostic on failure."""
        result = self.run_command("git", *args)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_prepare_ignores_missing_and_malformed_receipts(self):
        """Stopped preparation computes the tree without opening a receipt."""
        receipts = self.repo / ".git/storyhook/gate-receipts"
        for malformed in [False, True]:
            if malformed:
                (receipts / self.tree).mkdir(parents=True)
            result = self.run_command("bash", ROOT / "scripts/merge-preflight.sh", "--prepare-only", self.base, self.head)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), self.tree)
            self.assertEqual(list(receipts.iterdir()) if receipts.exists() else [],
                             [receipts / self.tree] if malformed else [])

    def prepared(self, expected, locked=True):
        """Use a filesystem witness after the production merge guard."""
        command = ["bash", ROOT / "scripts/land-pr.sh", "--prepared-run", self.base, self.head,
                   expected, "admitted-attempt", "--", "touch", self.root / "merged"]
        if locked:
            command = ["bash", ROOT / "scripts/machine-lock.sh", "merge", "--", *command]
        return self.run_command(*command, extra={
            "STORYHOOK_LANDING_HEAD": self.head,
            "STORYHOOK_LANDING_TREE": expected,
            "STORYHOOK_LANDING_ATTEMPT_MARKER": str(self.root / "landing.attempted"),
        })

    def test_prepared_merge_requires_the_exact_tree_and_lock(self):
        """Neither a changed tree nor an unlocked caller reaches the merge."""
        for expected, locked in [("0" * 40, True), (self.tree, False)]:
            result = self.prepared(expected, locked)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse((self.root / "merged").exists())
        result = self.prepared(self.tree)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.root / "merged").exists())
        self.assertFalse((self.repo / ".git/storyhook/gate-receipts" / self.tree).exists())

    def test_ordinary_landing_still_requires_certification(self):
        """A stopped-mode environment cannot alter the ordinary entry point."""
        result = self.run_command(
            "bash", ROOT / "scripts/machine-lock.sh", "merge", "--", "bash",
            ROOT / "scripts/land-pr.sh", "--certified-run", self.base, self.head,
            "--", "touch", self.root / "merged",
        )
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.root / "merged").exists())


if __name__ == "__main__":
    unittest.main()
