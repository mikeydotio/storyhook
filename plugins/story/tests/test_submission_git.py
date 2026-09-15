"""SH-725: production submission Git resolves credentials without a network."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


LIB = Path(__file__).resolve().parents[1] / "lib" / "submission-git.sh"


class SubmissionGitTests(unittest.TestCase):
    """Real Git configuration and credential protocol with a fixture gh endpoint."""

    def setUp(self):
        """Create a private home, repository, and credential endpoints."""
        self.temp = tempfile.TemporaryDirectory(prefix="story-submit-auth-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "HOME": str(self.root),
            "XDG_CONFIG_HOME": str(self.root / "config"),
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": str(self.root / "global.gitconfig"),
            "GIT_TERMINAL_PROMPT": "0",
            "GH_PROMPT_DISABLED": "1",
            "GH_TOKEN": "fixture-token-not-a-real-credential",
            "GH_CONFIG_DIR": str(self.root / "gh"),
            "AUTH_TEST_ROOT": str(self.root),
        }
        self.write_executable("gh", '''#!/usr/bin/env bash
set -eu
[ "$1 $2 $3" = "auth git-credential get" ] || exit 91
printf '%s:%s:%s\\n' "$GIT_TERMINAL_PROMPT" "$GH_PROMPT_DISABLED" "$GH_CONFIG_DIR" >>"$AUTH_TEST_ROOT/calls"
cat >>"$AUTH_TEST_ROOT/request"
if [ -z "${GH_TOKEN:-}" ]; then exit 0; fi
printf 'username=fixture-user\\npassword=%s\\n' "$GH_TOKEN"
''')
        self.write_executable("inherited-helper", '''#!/usr/bin/env bash
cat >/dev/null
printf 'called\\n' >>"$AUTH_TEST_ROOT/inherited"
printf 'failed to get: -25308\\n' >&2
exit 1
''')
        self.git("init", "-q", "-b", "main")
        self.git("config", "--global", "credential.helper", "!inherited-helper")
        self.git("config", "credential.https://github.com.helper", "!inherited-helper")

    def write_executable(self, name, contents):
        """Install an isolated external credential endpoint."""
        path = self.bin / name
        path.write_text(contents)
        path.chmod(0o755)

    def git(self, *args):
        """Run fixture Git without inheriting user credentials or configuration."""
        return subprocess.run(
            ["git", *args], cwd=self.root, env=self.env,
            text=True, capture_output=True, check=True,
        ).stdout.strip()

    def submit_git(self, *args, input=None):
        """Exercise the shipped runner, bounded even when credentials are absent."""
        return subprocess.run(
            ["bash", "-c", 'source "$1"; shift; submission_git "$@"',
             "submission-test", str(LIB), *args],
            cwd=self.root, env=self.env, text=True, input=input,
            capture_output=True, timeout=10,
        )

    def test_github_bypasses_inherited_keychain_and_uses_gh(self):
        """A GitHub token must reach Git despite an unusable inherited helper."""
        result = self.submit_git("credential", "fill", input="protocol=https\nhost=github.com\n\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("password=" + self.env["GH_TOKEN"], result.stdout)
        self.assertFalse((self.root / "inherited").exists())
        self.assertIn("host=github.com", (self.root / "request").read_text())
        self.assertNotIn(self.env["GH_TOKEN"], result.stderr)

    def test_missing_credentials_fail_without_fallback_or_secret_output(self):
        """An absent token must fail without interactive fallback."""
        self.env.pop("GH_TOKEN")
        result = self.submit_git("credential", "fill", input="protocol=https\nhost=github.com\n\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("terminal prompts disabled", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertFalse((self.root / "inherited").exists())

    def test_other_hosts_and_http_keep_their_configured_helper(self):
        """GitHub authentication must not reach other credential contexts."""
        for url in ("https://example.com", "https://github.com.evil.invalid", "http://github.com"):
            with self.subTest(url=url):
                result = self.submit_git("credential", "fill", input=f"url={url}\n\n")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("-25308", result.stderr)
                self.assertFalse((self.root / "calls").exists())

    def test_missing_credentials_never_launch_askpass(self):
        """Terminal suppression must also exclude the separate GUI prompt paths."""
        self.env.pop("GH_TOKEN")
        self.write_executable("askpass", '''#!/usr/bin/env bash
printf 'called\\n' >>"$AUTH_TEST_ROOT/askpass-calls"
exit 1
''')
        askpass = str(self.bin / "askpass")
        for source in ("GIT_ASKPASS", "SSH_ASKPASS", "core.askPass"):
            with self.subTest(source=source):
                self.env.pop("GIT_ASKPASS", None)
                self.env.pop("SSH_ASKPASS", None)
                if source == "core.askPass":
                    self.git("config", source, askpass)
                else:
                    self.env[source] = askpass
                marker = self.root / "askpass-calls"
                marker.unlink(missing_ok=True)
                result = self.submit_git("credential", "fill", input="protocol=https\nhost=github.com\n\n")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("terminal prompts disabled", result.stderr)
                self.assertFalse(marker.exists(), f"{source} launched an interactive prompt")

    def test_prompt_guards_override_interactive_environment(self):
        """The production boundary must enforce both noninteractive guards."""
        self.env["GIT_TERMINAL_PROMPT"] = "1"
        self.env["GH_PROMPT_DISABLED"] = "0"
        result = self.submit_git("credential", "fill", input="protocol=https\nhost=github.com\n\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "calls").read_text(), f"0:1:{self.env['GH_CONFIG_DIR']}\n")

    def test_remote_forms_are_rewritten_only_for_github(self):
        """Both supported GitHub SSH forms use HTTPS without altering other URLs."""
        for original, expected in (
            ("git@github.com:acme/widgets.git", "https://github.com/acme/widgets.git"),
            ("ssh://git@github.com/acme/widgets.git", "https://github.com/acme/widgets.git"),
            ("https://github.com/acme/widgets.git", "https://github.com/acme/widgets.git"),
            ("git@example.com:acme/widgets.git", "git@example.com:acme/widgets.git"),
            (str(self.root / "origin.git"), str(self.root / "origin.git")),
        ):
            with self.subTest(original=original):
                self.git("config", "remote.origin.url", original)
                result = self.submit_git("remote", "get-url", "origin")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), expected)

    def test_persistent_configuration_is_unchanged(self):
        """Credential selection must leave user and repository configuration intact."""
        paths = [self.root / "global.gitconfig", self.root / ".git/config"]
        before = [path.read_bytes() for path in paths]
        self.submit_git("credential", "fill", input="protocol=https\nhost=github.com\n\n")
        self.assertEqual(before, [path.read_bytes() for path in paths])


if __name__ == "__main__":
    unittest.main()
