"""SH-859: only complete, path-bound workspace trust permits startup input."""

import json
import pathlib
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "lib"))
from startup_trust import TrustError, classify


def screen(provider, path, selected=True):
    """Render provider-owned data; production classification stays under test."""
    if provider == "codex":
        return (f"  Folder access\n\n  {path}\n\n  Trust this folder?\n\n"
                f"{'›' if selected else ' '} 1. Trust and continue\n"
                f"{' ' if selected else '›'} 2. Quit\n\n  enter continue · esc quit\n")
    return (f"╭─ Accessing workspace: ───╮\n│ {path} │\n│\n"
            "│ Quick safety check: Is this a project you created or one you trust?\n│\n"
            f"│ {' ' if selected else '❯'} No, exit\n"
            f"│ {'❯' if selected else ' '} Yes, I trust this folder\n"
            "│ enter confirm · esc cancel\n╰──────────────────────────╯\n")


class WorkspaceTrustScreens(unittest.TestCase):
    """Exercise parser boundaries independently of terminal scheduling."""

    def setUp(self):
        """Use real paths so aliases and unrelated directories are meaningful."""
        self.temp = tempfile.TemporaryDirectory(prefix="story-trust-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name).resolve()
        self.cwd = self.root / "worktree with  spaces-é"
        self.cwd.mkdir()

    def parse(self, provider, text):
        """Pass the dispatch-owned paths to the production parser."""
        return classify(provider, text, str(self.cwd), str(self.root))

    def test_selection_and_fingerprint(self):
        """Focus determines direction, while changing focus preserves identity."""
        for provider, direction in (("codex", "Up"), ("claude", "Down")):
            with self.subTest(provider=provider):
                yes = self.parse(provider, screen(provider, self.cwd))
                no = self.parse(provider, screen(provider, self.cwd, False))
                self.assertEqual(yes["key"], "Enter")
                self.assertEqual(no["key"], direction)
                self.assertEqual(yes["fingerprint"], no["fingerprint"])

    def test_clock_is_shared_across_parser_processes(self):
        """Shell polling compares separate processes, including macOS Python 3.9."""
        before = time.clock_gettime(time.CLOCK_MONOTONIC)
        output = subprocess.check_output(
            [sys.executable, "-B", str(pathlib.Path(__file__).resolve().parents[1] / "lib/startup_trust.py"),
             "codex", str(self.cwd), str(self.root)], input="", text=True)
        after = time.clock_gettime(time.CLOCK_MONOTONIC)
        self.assertLessEqual(before, json.loads(output)["now"])
        self.assertLessEqual(json.loads(output)["now"], after)

    def test_physical_path_wrap_is_ambiguous(self):
        """Only tmux soft-wrap joining can reconstruct a path without guessing."""
        for provider in ("codex", "claude"):
            with self.assertRaises(TrustError):
                self.parse(provider, screen(provider, str(self.cwd).replace("worktree", "work\ntree")))

    def test_navigation_uses_observed_order(self):
        """A negative selection above the affirmative requires Down, not a toggle."""
        text = screen("codex", self.cwd, False).replace(
            "  1. Trust and continue\n› 2. Quit", "› 1. Quit\n  2. Trust and continue")
        self.assertEqual(self.parse("codex", text)["key"], "Down")

    def test_ansi_and_canonical_alias(self):
        """Styling and a real symlink do not change directory ownership."""
        alias = self.root / "alias"
        alias.symlink_to(self.cwd, target_is_directory=True)
        for provider in ("codex", "claude"):
            text = screen(provider, alias).replace("Yes,", "\x1b[1mYes,").replace("folder\n", "folder\x1b[0m\n")
            self.assertEqual(self.parse(provider, text)["key"], "Enter")

    def test_rejects_unrelated_or_ambiguous_paths(self):
        """Neither a shared parent nor a path prefix establishes authority."""
        other = self.root / "other"
        other.mkdir()
        for provider in ("codex", "claude"):
            for path in (other, self.root, str(self.cwd) + "-other", "…/worktree", "~/project", "relative"):
                with self.subTest(provider=provider, path=path):
                    with self.assertRaises(TrustError):
                        self.parse(provider, screen(provider, path))

    def test_codex_explicit_repository_root(self):
        """An explicit root disclosure may name exactly this repository."""
        text = screen("codex", self.cwd) + (
            "Note: You’re in a subdirectory of a Git project. Trusting will apply to the repository root:\n"
            f"{self.root}\n")
        self.assertEqual(self.parse("codex", text)["key"], "Enter")
        with self.assertRaises(TrustError):
            self.parse("codex", text.replace(str(self.root) + "\n", "/different\n"))

    def test_malformed_trust_dialogs_are_not_readiness(self):
        """Incomplete, duplicate or unselected choices never authorize input."""
        for provider in ("codex", "claude"):
            good = screen(provider, self.cwd)
            cursor = "›" if provider == "codex" else "❯"
            yes = "Trust and continue" if provider == "codex" else "Yes, I trust this folder"
            for bad in (good.replace(cursor, " "), good.replace(yes, "Approve everything"),
                        good + f"{cursor} {yes}\n", good.replace(str(self.cwd), "")):
                with self.subTest(provider=provider, screen=bad):
                    with self.assertRaises(TrustError):
                        self.parse(provider, bad)

    def test_foreign_provider_and_other_dialogs(self):
        """Only workspace trust belongs to this startup handler."""
        unrelated = ("Login required\n", "Update available\n", "❯ Yes, I trust these settings\n",
                     "Would you like to run this command?\n› 1. Yes, proceed\n2. No\n",
                     "──────────────────\n› Ask Codex to do anything\n? for shortcuts\n")
        for provider in ("codex", "claude"):
            for text in unrelated:
                with self.subTest(provider=provider, text=text):
                    self.assertIsNone(self.parse(provider, text))
            other = "codex" if provider == "claude" else "claude"
            with self.assertRaises(TrustError):
                self.parse(provider, screen(other, self.cwd))


if __name__ == "__main__":
    unittest.main()
