"""Submission transport uses real Git credentials at a controlled network boundary."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


LIB = Path(__file__).resolve().parents[1] / "lib" / "submission-git.sh"
REAL_GIT = shutil.which("git")


class SubmissionGitTests(unittest.TestCase):
    """Real origin validation and credential protocol with external endpoints replaced."""

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
            "GH_TOKEN": "fixture-public-credential",
            "GH_ENTERPRISE_TOKEN": "fixture-enterprise-credential",
            "GH_CONFIG_DIR": str(self.root),
        }
        # Preserve the shell harness's containment even though this local helper
        # should never start a daemon.
        for name in ("STORYHOOK_DAEMON_ADDR", "STORYHOOK_PARENT_PID", "STORY_BIN"):
            if name in os.environ:
                self.env[name] = os.environ[name]
        self.write_executable("gh", '''#!/usr/bin/env bash
set -eu
[ "$1 $2 $3" = "auth git-credential get" ] || exit 91
printf '%s:%s:%s\\n' "$GIT_TERMINAL_PROMPT" "$GH_PROMPT_DISABLED" "$GH_CONFIG_DIR" >>"$GH_CONFIG_DIR/calls"
cat >>"$GH_CONFIG_DIR/request"
case "$GH_HOST" in
  github.com) token="${GH_TOKEN:-}" ;;
  *) token="${GH_ENTERPRISE_TOKEN:-}" ;;
esac
[ -n "$token" ] || exit 0
printf 'username=fixture-user\\npassword=%s\\n' "$token"
''')
        self.write_executable("inherited-helper", '''#!/usr/bin/env bash
cat >/dev/null
printf 'called\\n' >>"$HOME/inherited"
printf 'failed to get: -25308\\n' >&2
exit 1
''')
        # Only a network ls-remote is substituted. Real Git handles config,
        # origin discovery, rewrites, credential matching, helper order and
        # prompt suppression with the exact production -c arguments.
        self.write_executable("git", f'''#!{sys.executable}
import os
from pathlib import Path
import subprocess
import sys
args = sys.argv[1:]
real_git = {REAL_GIT!r}
if "ls-remote" not in args or "--get-url" in args:
    os.execv(real_git, [real_git, *args])
index = args.index("ls-remote")
urls = [arg for arg in args[index + 1:] if arg.startswith("https://")]
if len(urls) != 1:
    sys.exit("fixture network endpoint requires one HTTPS destination")
url = urls[0]
root = Path(os.environ["GH_CONFIG_DIR"])
(root / "destination").write_text(url)
result = subprocess.run([real_git, *args[:index], "credential", "fill"],
    input="url=" + url + "\\n\\n", text=True, capture_output=True)
if result.returncode:
    sys.stderr.write(result.stderr)
    sys.exit(result.returncode)
host = url.split("/")[2]
expected = os.environ.get("GH_TOKEN" if host == "github.com" else "GH_ENTERPRISE_TOKEN", "")
if not expected or "password=" + expected not in result.stdout:
    sys.exit("fixture endpoint received the wrong host credential")
print("a" * 40 + "\\trefs/heads/main")
''')
        self.git("init", "-q", "-b", "main")
        self.git("remote", "add", "origin", "git@github.com:acme/widgets.git")
        self.git("config", "--global", "credential.helper", "!inherited-helper")
        for host in ("github.com", "github.example.com"):
            self.git("config", f"credential.https://{host}.helper", "!inherited-helper")

    def write_executable(self, name, contents):
        """Install an isolated external endpoint."""
        path = self.bin / name
        path.write_text(contents)
        path.chmod(0o755)

    def git(self, *args):
        """Run fixture Git without inheriting user credentials or configuration."""
        return subprocess.run(
            [REAL_GIT, *args], cwd=self.root, env=self.env,
            text=True, capture_output=True, check=True,
        ).stdout.strip()

    def submit_git(self, *args):
        """Exercise the shipped runner, bounded even when credentials are absent."""
        return subprocess.run(
            ["bash", "-c", 'source "$1"; shift; submission_git "$@"',
             "submission-test", str(LIB), *args],
            cwd=self.root, env=self.env, text=True,
            capture_output=True, timeout=10,
        )

    def remote_head(self):
        """Read one ref through the actual submission API."""
        return self.submit_git("ls-remote", "--heads", "origin", "refs/heads/main")

    def test_github_bypasses_inherited_keychain_and_uses_gh(self):
        """Both hosts use gh credentials without exposing them to the caller."""
        for host in ("github.com", "github.example.com"):
            with self.subTest(host=host):
                self.git("remote", "set-url", "origin", f"git@{host}:acme/widgets.git")
                result = self.remote_head()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "a" * 40 + "\trefs/heads/main\n")
                self.assertFalse((self.root / "inherited").exists())
                self.assertIn(f"host={host}", (self.root / "request").read_text())
                for name in ("GH_TOKEN", "GH_ENTERPRISE_TOKEN"):
                    self.assertNotIn(self.env[name], result.stdout + result.stderr)

    def test_missing_credentials_fail_without_fallback_or_secret_output(self):
        """A public token cannot authenticate an Enterprise origin."""
        self.env.pop("GH_ENTERPRISE_TOKEN")
        self.git("remote", "set-url", "origin", "git@github.example.com:acme/widgets.git")
        result = self.remote_head()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("terminal prompts disabled", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertNotIn(self.env["GH_TOKEN"], result.stderr)
        self.assertFalse((self.root / "inherited").exists())

    def test_invalid_origins_and_foreign_operands_never_request_credentials(self):
        """Submission requires current GitHub authority before accessing credentials."""
        for origin in ("http://github.com/acme/widgets", str(self.root / "origin.git")):
            with self.subTest(origin=origin):
                self.git("remote", "set-url", "origin", origin)
                self.assertNotEqual(self.remote_head().returncode, 0)
                self.assertFalse((self.root / "calls").exists())
        self.git("remote", "set-url", "origin", "https://github.com/acme/widgets.git")
        for args in (("credential", "fill"), ("ls-remote", "https://foreign.invalid/acme/widgets.git")):
            with self.subTest(args=args):
                self.assertNotEqual(self.submit_git(*args).returncode, 0)
                self.assertFalse((self.root / "calls").exists())
                self.assertFalse((self.root / "destination").exists())

    def test_missing_credentials_never_launch_askpass(self):
        """Terminal suppression must also exclude the separate GUI prompt paths."""
        self.env.pop("GH_TOKEN")
        self.write_executable("askpass", '''#!/usr/bin/env bash
printf 'called\\n' >>"$HOME/askpass-calls"
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
                result = self.remote_head()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("terminal prompts disabled", result.stderr)
                self.assertFalse((self.root / "askpass-calls").exists())

    def test_prompt_guards_override_interactive_environment(self):
        """The production boundary must enforce both noninteractive guards."""
        self.env["GIT_TERMINAL_PROMPT"] = "1"
        self.env["GH_PROMPT_DISABLED"] = "0"
        result = self.remote_head()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "calls").read_text(), f"0:1:{self.env['GH_CONFIG_DIR']}\n")

    def test_remote_forms_use_https_and_preserve_host(self):
        """Supported public and Enterprise origins reach their explicit HTTPS endpoint."""
        for host in ("github.com", "github.example.com"):
            for original in (f"git@{host}:acme/widgets.git", f"ssh://git@{host}/acme/widgets.git", f"https://{host}/acme/widgets.git"):
                with self.subTest(original=original):
                    self.git("remote", "set-url", "origin", original)
                    result = self.remote_head()
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual((self.root / "destination").read_text(), f"https://{host}/acme/widgets.git")

    def test_persistent_configuration_is_unchanged(self):
        """Credential selection must leave user and repository configuration intact."""
        paths = [self.root / "global.gitconfig", self.root / ".git/config"]
        before = [path.read_bytes() for path in paths]
        result = self.remote_head()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(before, [path.read_bytes() for path in paths])


if __name__ == "__main__":
    unittest.main()
